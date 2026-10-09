//! The `IDataObject` that File Explorer pastes from, and the `IStream` of
//! each file (SPEC.md section 10, Phase 4). Windows only.
//!
//! Threads: the data object lives on the UI thread, which is an OLE STA.
//! Explorer calls it there through the message loop. Each stream lives in
//! the process MTA, so Explorer's `Read` calls run on COM worker threads
//! and wait there for the device thread, never on the UI thread. The
//! device thread itself never touches OLE.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Duration;

use windows::Win32::Foundation::{
    DATA_S_SAMEFORMATETC, DV_E_DVASPECT, DV_E_FORMATETC, DV_E_LINDEX, DV_E_TYMED, E_ABORT, E_FAIL,
    E_NOTIMPL, E_POINTER, GlobalFree, OLE_E_ADVISENOTSUPPORTED, S_FALSE, S_OK, STG_E_ACCESSDENIED,
};
use windows::Win32::System::Com::Marshal::CoMarshalInterThreadInterfaceInStream;
use windows::Win32::System::Com::StructuredStorage::CoGetInterfaceAndReleaseStream;
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoIncrementMTAUsage, CoInitializeEx, CoTaskMemAlloc, CoUninitialize,
    DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IBindCtx, IDataObject, IDataObject_Impl,
    IEnumFORMATETC, IEnumSTATDATA, ISequentialStream_Impl, IStream, IStream_Impl, LOCKTYPE,
    STATFLAG, STATFLAG_NONAME, STATSTG, STGC, STGMEDIUM, STGMEDIUM_0, STGTY_STREAM, STREAM_SEEK,
    STREAM_SEEK_CUR, STREAM_SEEK_SET, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, OleSetClipboard, ReleaseStgMedium};
use windows::Win32::UI::Shell::{
    CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, CFSTR_PERFORMEDDROPEFFECT,
    CFSTR_PREFERREDDROPEFFECT, FILEDESCRIPTORW, IDataObjectAsyncCapability,
    IDataObjectAsyncCapability_Impl, SHCreateStdEnumFmtEtc,
};
use windows::core::{BOOL, HRESULT, Interface, PWSTR, Ref, Result, implement};

use super::chunks::{self, ChunkReader, Pending};
use super::device::{ListingResult, Request};
use super::filedesc::{self, PasteFile};

/// How long `GetData` waits for the folder walk of a drag.
const LISTING_WAIT: Duration = Duration::from_secs(120);

// The portable layout in `filedesc` must match the Windows header.
const _: () = assert!(std::mem::size_of::<FILEDESCRIPTORW>() == filedesc::DESCRIPTOR_SIZE);
const _: () = assert!(std::mem::offset_of!(FILEDESCRIPTORW, cFileName) == 72);
const _: () = assert!(windows::Win32::UI::Shell::FD_FILESIZE.0 as u32 == filedesc::FD_FILESIZE);
const _: () = assert!(windows::Win32::UI::Shell::FD_UNICODE.0 as u32 == filedesc::FD_UNICODE);
const _: () = assert!(windows::Win32::UI::Shell::FD_PROGRESSUI.0 as u32 == filedesc::FD_PROGRESSUI);

/// The registered clipboard format ids.
#[derive(Clone, Copy)]
struct Formats {
    descriptor: u16,
    contents: u16,
    preferred: u16,
    performed: u16,
}

fn formats() -> Formats {
    static FORMATS: OnceLock<Formats> = OnceLock::new();
    *FORMATS.get_or_init(|| {
        // SAFETY: the format names are static NUL-terminated strings.
        let reg = |name| unsafe { RegisterClipboardFormatW(name) } as u16;
        Formats {
            descriptor: reg(CFSTR_FILEDESCRIPTORW),
            contents: reg(CFSTR_FILECONTENTS),
            preferred: reg(CFSTR_PREFERREDDROPEFFECT),
            performed: reg(CFSTR_PERFORMEDDROPEFFECT),
        }
    })
}

/// The name of a clipboard format for the logs.
fn format_name(cf: u16) -> String {
    let f = formats();
    match cf {
        _ if cf == f.descriptor => "FileGroupDescriptorW".into(),
        _ if cf == f.contents => "FileContents".into(),
        _ if cf == f.preferred => "Preferred DropEffect".into(),
        _ if cf == f.performed => "Performed DropEffect".into(),
        _ => format!("cf {cf}"),
    }
}

fn log_request(call: &str, fmt: &FORMATETC, result: HRESULT) {
    tracing::debug!(
        "paste: {call} {} aspect={} lindex={} tymed={:#x} -> {:#010x}",
        format_name(fmt.cfFormat),
        fmt.dwAspect,
        fmt.lindex,
        fmt.tymed,
        result.0
    );
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// State that the data objects and streams of the app share.
pub struct PasteShared {
    device: Sender<Request>,
    /// The stream that reads now. A new stream stops it.
    active: Mutex<Option<Weak<StreamCore>>>,
    /// Streams that Explorer holds.
    open: AtomicUsize,
    /// Explorer runs an asynchronous paste (`StartOperation`).
    operations: AtomicUsize,
}

impl PasteShared {
    pub fn new(device: Sender<Request>) -> Arc<Self> {
        Arc::new(Self {
            device,
            active: Mutex::new(None),
            open: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
        })
    }

    /// True while Explorer holds a stream or runs a paste.
    pub fn busy(&self) -> bool {
        self.open.load(Ordering::SeqCst) > 0 || self.operations.load(Ordering::SeqCst) > 0
    }

    /// Make `core` the reading stream and stop the one before it.
    fn activate(&self, core: &Arc<StreamCore>) {
        let previous = lock(&self.active).replace(Arc::downgrade(core));
        if let Some(old) = previous.and_then(|w| w.upgrade())
            && !Arc::ptr_eq(&old, core)
        {
            old.abort();
        }
    }
}

/// The data object of one copy or drag.
#[implement(IDataObject, IDataObjectAsyncCapability)]
pub struct DataObject {
    listing: Arc<Pending<ListingResult>>,
    shared: Arc<PasteShared>,
    async_mode: AtomicBool,
    in_operation: AtomicBool,
}

impl DataObject {
    /// A data object for the listing that the device thread puts into
    /// `listing`.
    pub fn create(listing: Arc<Pending<ListingResult>>, shared: Arc<PasteShared>) -> IDataObject {
        Self {
            listing,
            shared,
            async_mode: AtomicBool::new(true),
            in_operation: AtomicBool::new(false),
        }
        .into()
    }

    fn listing(&self) -> Result<Arc<filedesc::Listing>> {
        match self.listing.wait(LISTING_WAIT) {
            Some(Ok(l)) => Ok(l),
            Some(Err(e)) => {
                tracing::warn!("paste: cannot list the selection: {e}");
                Err(E_FAIL.into())
            }
            None => {
                tracing::warn!("paste: the folder walk took too long");
                Err(E_FAIL.into())
            }
        }
    }

    fn stream(&self, file: &PasteFile, files: usize) -> Result<IStream> {
        let core = Arc::new(StreamCore {
            file: file.clone(),
            files,
            shared: Arc::clone(&self.shared),
            state: Mutex::new(State::Unopened),
            aborted: AtomicBool::new(false),
        });
        mta_stream(core)
    }
}

/// Check the aspect and the medium of a request.
fn check(fmt: &FORMATETC, tymed: i32) -> HRESULT {
    if fmt.dwAspect != DVASPECT_CONTENT.0 {
        DV_E_DVASPECT
    } else if fmt.tymed & tymed as u32 == 0 {
        DV_E_TYMED
    } else {
        S_OK
    }
}

/// The medium that a format is offered on, or `None` if it is not offered.
fn offered(cf: u16) -> Option<i32> {
    let f = formats();
    if cf == f.descriptor || cf == f.preferred {
        Some(TYMED_HGLOBAL.0)
    } else if cf == f.contents {
        Some(TYMED_ISTREAM.0)
    } else {
        None
    }
}

fn hglobal_medium(bytes: &[u8]) -> Result<STGMEDIUM> {
    // SAFETY: the block is at least `bytes.len()` long and locked while
    // the bytes are copied.
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let p = GlobalLock(h);
        if p.is_null() {
            let _ = GlobalFree(Some(h));
            return Err(E_FAIL.into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(h);
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: h },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        })
    }
}

fn format(cf: u16, tymed: i32) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: tymed as u32,
    }
}

impl DataObject {
    fn get_data(&self, fmt: &FORMATETC) -> Result<STGMEDIUM> {
        let tymed = offered(fmt.cfFormat).ok_or(DV_E_FORMATETC)?;
        check(fmt, tymed).ok()?;
        let f = formats();
        if fmt.cfFormat == f.preferred {
            return hglobal_medium(&DROPEFFECT_COPY.0.to_le_bytes());
        }
        let listing = self.listing()?;
        if fmt.cfFormat == f.descriptor {
            return hglobal_medium(&listing.group);
        }
        let file = listing.file(fmt.lindex).ok_or(DV_E_LINDEX)?;
        let stream = self.stream(file, listing.files)?;
        Ok(STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 {
                pstm: std::mem::ManuallyDrop::new(Some(stream)),
            },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        })
    }
}

impl IDataObject_Impl for DataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> Result<STGMEDIUM> {
        // SAFETY: COM passes a valid pointer or null.
        let fmt = unsafe { pformatetcin.as_ref() }.ok_or(E_POINTER)?;
        let result = self.get_data(fmt);
        let code = match &result {
            Ok(_) => S_OK,
            Err(e) => e.code(),
        };
        log_request("GetData", fmt, code);
        result
    }

    fn GetDataHere(&self, _pformatetc: *const FORMATETC, _pmedium: *mut STGMEDIUM) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        // SAFETY: COM passes a valid pointer or null.
        let Some(fmt) = (unsafe { pformatetc.as_ref() }) else {
            return E_POINTER;
        };
        let result = match offered(fmt.cfFormat) {
            Some(tymed) => check(fmt, tymed),
            None => DV_E_FORMATETC,
        };
        log_request("QueryGetData", fmt, result);
        result
    }

    fn GetCanonicalFormatEtc(
        &self,
        pformatectin: *const FORMATETC,
        pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        // SAFETY: COM passes valid pointers or null.
        let (Some(input), Some(out)) = (unsafe { pformatectin.as_ref() }, unsafe {
            pformatetcout.as_mut()
        }) else {
            return E_POINTER;
        };
        *out = *input;
        out.ptd = std::ptr::null_mut();
        DATA_S_SAMEFORMATETC
    }

    fn SetData(
        &self,
        pformatetc: *const FORMATETC,
        pmedium: *const STGMEDIUM,
        frelease: BOOL,
    ) -> Result<()> {
        // SAFETY: COM passes a valid pointer or null.
        let fmt = unsafe { pformatetc.as_ref() }.ok_or(E_POINTER)?;
        // Explorer reports the performed effect here. It is always a copy.
        if fmt.cfFormat != formats().performed {
            return Err(E_NOTIMPL.into());
        }
        if frelease.as_bool() && !pmedium.is_null() {
            // SAFETY: with `frelease` this object owns the medium.
            unsafe { ReleaseStgMedium(pmedium.cast_mut()) };
        }
        Ok(())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> Result<IEnumFORMATETC> {
        tracing::debug!("paste: EnumFormatEtc direction={dwdirection}");
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        let f = formats();
        let list = [
            format(f.descriptor, TYMED_HGLOBAL.0),
            format(f.contents, TYMED_ISTREAM.0),
            format(f.preferred, TYMED_HGLOBAL.0),
        ];
        // SAFETY: the array outlives the call; the enumerator copies it.
        unsafe { SHCreateStdEnumFmtEtc(&list) }
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: Ref<IAdviseSink>,
    ) -> Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _dwconnection: u32) -> Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

/// Lets Explorer paste in the background, so a drop does not hold the
/// drag loop of the UI thread until the copy ends.
impl IDataObjectAsyncCapability_Impl for DataObject_Impl {
    fn SetAsyncMode(&self, fdoopasync: BOOL) -> Result<()> {
        self.async_mode
            .store(fdoopasync.as_bool(), Ordering::SeqCst);
        Ok(())
    }

    fn GetAsyncMode(&self) -> Result<BOOL> {
        Ok(self.async_mode.load(Ordering::SeqCst).into())
    }

    fn StartOperation(&self, _pbcreserved: Ref<IBindCtx>) -> Result<()> {
        if !self.in_operation.swap(true, Ordering::SeqCst) {
            self.shared.operations.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    fn InOperation(&self) -> Result<BOOL> {
        Ok(self.in_operation.load(Ordering::SeqCst).into())
    }

    fn EndOperation(
        &self,
        _hresult: HRESULT,
        _pbcreserved: Ref<IBindCtx>,
        _dweffects: u32,
    ) -> Result<()> {
        if self.in_operation.swap(false, Ordering::SeqCst) {
            self.shared.operations.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

impl Drop for DataObject {
    fn drop(&mut self) {
        if *self.in_operation.get_mut() {
            self.shared.operations.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

enum State {
    /// No byte was asked for yet. The device thread has no request.
    Unopened,
    Reading(ChunkReader),
    Closed,
}

/// The state of one file stream, shared with `PasteShared::active`.
struct StreamCore {
    file: PasteFile,
    files: usize,
    shared: Arc<PasteShared>,
    state: Mutex<State>,
    aborted: AtomicBool,
}

impl StreamCore {
    /// Stop the read. The device thread sees the closed channel and stops
    /// `read_to`.
    fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
        // A `Read` that holds the lock drops the reader when it returns.
        if let Ok(mut s) = self.state.try_lock() {
            *s = State::Closed;
        }
    }

    /// Read into `buf`. Returns the count and true at the end of the file.
    fn read(self: &Arc<Self>, buf: &mut [u8]) -> std::result::Result<usize, HRESULT> {
        if self.aborted.load(Ordering::SeqCst) {
            return Err(E_ABORT);
        }
        let mut state = lock(&self.state);
        if matches!(*state, State::Unopened) {
            self.shared.activate(self);
            let (tx, rx) = chunks::channel();
            let request = Request::OpenRead {
                path: self.file.path.clone(),
                number: self.file.number,
                files: self.files,
                tx,
            };
            if self.shared.device.send(request).is_err() {
                *state = State::Closed;
                return Err(E_FAIL);
            }
            *state = State::Reading(ChunkReader::new(rx));
        }
        let result = match &mut *state {
            State::Reading(reader) => reader.read(buf).map_err(|e| {
                tracing::warn!("paste: {}: {e}", self.file.path);
                E_FAIL
            }),
            State::Closed => Err(E_ABORT),
            State::Unopened => unreachable!("opened above"),
        };
        if self.aborted.load(Ordering::SeqCst) || result.is_err() {
            *state = State::Closed;
        }
        result
    }

    fn started(&self) -> bool {
        !matches!(*lock(&self.state), State::Unopened)
    }
}

/// The `IStream` of one file.
#[implement(IStream)]
struct PasteStream {
    core: Arc<StreamCore>,
}

impl Drop for PasteStream {
    fn drop(&mut self) {
        self.core.abort();
        self.core.shared.open.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ISequentialStream_Impl for PasteStream_Impl {
    fn Read(&self, pv: *mut c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        if pv.is_null() {
            return E_POINTER;
        }
        // SAFETY: COM gives a buffer of `cb` bytes at `pv`.
        let buf = unsafe { std::slice::from_raw_parts_mut(pv.cast::<u8>(), cb as usize) };
        let (n, hr) = match self.core.read(buf) {
            Ok(n) if n == buf.len() => (n, S_OK),
            Ok(n) => (n, S_FALSE),
            Err(hr) => (0, hr),
        };
        if !pcbread.is_null() {
            // SAFETY: a non-null `pcbread` points to a u32.
            unsafe { *pcbread = n as u32 };
        }
        hr
    }

    fn Write(&self, _pv: *const c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        STG_E_ACCESSDENIED
    }
}

impl IStream_Impl for PasteStream_Impl {
    fn Seek(&self, dlibmove: i64, dworigin: STREAM_SEEK, plibnewposition: *mut u64) -> Result<()> {
        // Only "rewind before the first read" and "where am I at the start".
        let ok = dlibmove == 0
            && (dworigin == STREAM_SEEK_SET || dworigin == STREAM_SEEK_CUR)
            && !self.core.started();
        if !ok {
            return Err(E_NOTIMPL.into());
        }
        if !plibnewposition.is_null() {
            // SAFETY: a non-null pointer points to a u64.
            unsafe { *plibnewposition = 0 };
        }
        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> Result<()> {
        Err(STG_E_ACCESSDENIED.into())
    }

    fn CopyTo(
        &self,
        pstm: Ref<IStream>,
        cb: u64,
        pcbread: *mut u64,
        pcbwritten: *mut u64,
    ) -> Result<()> {
        let target = pstm.ok()?;
        let mut buf = vec![0u8; chunks::CHUNK_SIZE];
        let (mut read, mut written) = (0u64, 0u64);
        let mut result = Ok(());
        while read < cb {
            let want = (cb - read).min(buf.len() as u64) as usize;
            let n = match self.core.read(&mut buf[..want]) {
                Ok(n) => n,
                Err(hr) => {
                    result = Err(hr.into());
                    break;
                }
            };
            read += n as u64;
            let mut done = 0u32;
            // SAFETY: `buf` holds `n` valid bytes.
            let hr = unsafe { target.Write(buf.as_ptr().cast(), n as u32, Some(&mut done)) };
            written += done as u64;
            if hr.is_err() {
                result = Err(hr.into());
                break;
            }
            if n < want {
                break;
            }
        }
        // SAFETY: non-null out pointers point to u64 values.
        unsafe {
            if !pcbread.is_null() {
                *pcbread = read;
            }
            if !pcbwritten.is_null() {
                *pcbwritten = written;
            }
        }
        result
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> Result<()> {
        Ok(())
    }

    fn Revert(&self) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn LockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: &LOCKTYPE) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn UnlockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Stat(&self, pstatstg: *mut STATSTG, grfstatflag: &STATFLAG) -> Result<()> {
        // SAFETY: COM passes a valid pointer or null.
        let stat = unsafe { pstatstg.as_mut() }.ok_or(E_POINTER)?;
        *stat = STATSTG {
            r#type: STGTY_STREAM.0 as u32,
            cbSize: self.core.file.size.unwrap_or(0),
            ..Default::default()
        };
        if grfstatflag.0 & STATFLAG_NONAME.0 == 0 {
            stat.pwcsName = co_task_string(&self.core.file.name);
        }
        Ok(())
    }

    fn Clone(&self) -> Result<IStream> {
        Err(E_NOTIMPL.into())
    }
}

/// A NUL-terminated copy of `s` from `CoTaskMemAlloc`; the caller frees it.
fn co_task_string(s: &str) -> PWSTR {
    let units: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the block holds `units` and is written once.
    unsafe {
        let p = CoTaskMemAlloc(units.len() * 2).cast::<u16>();
        if p.is_null() {
            return PWSTR::null();
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
        PWSTR(p)
    }
}

/// Keep the process MTA alive, so streams made there stay reachable.
fn ensure_mta() {
    static MTA: OnceLock<bool> = OnceLock::new();
    MTA.get_or_init(|| {
        // SAFETY: plain call; the cookie is never released, so the MTA
        // lives until the process ends.
        match unsafe { CoIncrementMTAUsage() } {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("paste: CoIncrementMTAUsage failed: {e}");
                false
            }
        }
    });
}

/// An interface pointer that may move to another thread: the marshaled
/// stream of `CoMarshalInterThreadInterfaceInStream` is free-threaded.
struct Marshaled(IStream);
// SAFETY: see above.
unsafe impl Send for Marshaled {}

/// Make the stream object in the MTA and return a proxy to it for the
/// UI thread. Explorer's calls then go to the MTA, not to the UI thread.
fn mta_stream(core: Arc<StreamCore>) -> Result<IStream> {
    ensure_mta();
    core.shared.open.fetch_add(1, Ordering::SeqCst);
    let made = std::thread::spawn(move || -> Result<Marshaled> {
        // SAFETY: balanced with CoUninitialize below on this thread. The
        // MTA usage count keeps the stub alive after it.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        let stream: IStream = PasteStream { core }.into();
        // SAFETY: `stream` is a valid interface of this apartment.
        let marshaled = unsafe { CoMarshalInterThreadInterfaceInStream(&IStream::IID, &stream) };
        drop(stream);
        // SAFETY: matches the CoInitializeEx above.
        unsafe { CoUninitialize() };
        marshaled.map(Marshaled)
    })
    .join()
    .map_err(|_| windows::core::Error::from(E_FAIL))??;
    // SAFETY: the call releases the marshal stream, so `forget` keeps the
    // wrapper from releasing it a second time.
    let proxy = unsafe { CoGetInterfaceAndReleaseStream::<_, IStream>(&made.0) };
    std::mem::forget(made);
    proxy
}

/// `S_OK` if `obj` is on the clipboard now.
fn is_current_clipboard(obj: &IDataObject) -> HRESULT {
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn OleIsCurrentClipboard(pdataobj: *mut c_void) -> HRESULT;
    }
    // SAFETY: `obj` is a valid interface; OleIsCurrentClipboard only reads it.
    unsafe { OleIsCurrentClipboard(obj.as_raw()) }
}

/// Put `obj` on the clipboard.
pub fn set_clipboard(obj: &IDataObject) -> Result<()> {
    // SAFETY: called on the UI thread, which is an OLE STA.
    unsafe { OleSetClipboard(obj) }?;
    let current = is_current_clipboard(obj);
    tracing::info!(
        "clipboard set; OleIsCurrentClipboard -> {:#010x}",
        current.0
    );
    Ok(())
}

/// Take `obj` off the clipboard if it is still there.
pub fn clear_clipboard(obj: &IDataObject) {
    let current = is_current_clipboard(obj);
    if current == S_OK {
        // SAFETY: called on the UI thread, which is an OLE STA.
        if let Err(e) = unsafe { OleSetClipboard(None) } {
            tracing::warn!("cannot clear the clipboard: {e}");
        }
    }
}

/// Initialize OLE on the UI thread. Returns false if it failed.
pub fn ole_init() -> bool {
    // SAFETY: plain call on the UI thread before any window exists; the
    // matching `ole_uninit` runs after the event loop on the same thread.
    match unsafe { windows::Win32::System::Ole::OleInitialize(None) } {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("OleInitialize failed: {e}");
            false
        }
    }
}

pub fn ole_uninit() {
    // SAFETY: matches a successful `ole_init` on this thread.
    unsafe { windows::Win32::System::Ole::OleUninitialize() };
}
