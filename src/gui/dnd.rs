//! Drag and drop to File Explorer with `DoDragDrop`. Windows only.

use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK,
};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource, IDropSource_Impl,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::core::{BOOL, HRESULT, implement};

/// Ends the drag on Escape (cancel) or when the left button goes up (drop).
#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            DRAGDROP_S_CANCEL
        } else if grfkeystate.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Run the drag loop with `obj`. Copy is the only allowed effect. Returns
/// true if the user dropped it on a target that accepted it.
///
/// `DoDragDrop` runs a nested message loop on the UI thread and returns
/// when the drag ends.
pub fn drag(obj: &IDataObject) -> windows::core::Result<bool> {
    let source: IDropSource = DropSource.into();
    let mut effect = DROPEFFECT_NONE;
    // SAFETY: called on the UI thread, which is an OLE STA. `effect`
    // outlives the call.
    let hr = unsafe { DoDragDrop(obj, &source, DROPEFFECT_COPY, &mut effect) };
    hr.ok()?;
    Ok(hr == DRAGDROP_S_DROP && effect != DROPEFFECT_NONE)
}
