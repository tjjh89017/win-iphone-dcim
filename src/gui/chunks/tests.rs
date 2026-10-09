use std::sync::Arc;

use super::*;

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

#[test]
fn writer_to_reader_round_trip() {
    let (tx, rx) = channel();
    let source = data(CHUNK_SIZE * 3 + 1234);
    let expected = source.clone();
    let writer = std::thread::spawn(move || {
        let mut total = 0;
        let mut w = ChunkWriter::new(tx, |n| total += n);
        for part in source.chunks(100_000) {
            w.write_all(part).unwrap();
        }
        w.finish().unwrap();
        total
    });
    let mut reader = ChunkReader::new(rx);
    let mut got = Vec::new();
    let mut buf = vec![0; 70_000];
    loop {
        let n = reader.read(&mut buf).unwrap();
        got.extend_from_slice(&buf[..n]);
        if n < buf.len() {
            break;
        }
    }
    assert!(reader.is_done());
    assert_eq!(got, expected);
    assert_eq!(writer.join().unwrap(), expected.len() as u64);
    // After the end, reads return 0.
    assert_eq!(reader.read(&mut buf).unwrap(), 0);
}

#[test]
fn the_channel_is_bounded() {
    let (tx, rx) = channel();
    let mut w = ChunkWriter::new(tx.clone(), |_| {});
    for _ in 0..CHUNK_QUEUE {
        w.write_all(&data(CHUNK_SIZE)).unwrap();
    }
    assert!(matches!(
        tx.try_send(Chunk::End),
        Err(TrySendError::Full(_))
    ));
    drop(rx);
}

#[test]
fn a_closed_reader_fails_the_writer() {
    let (tx, rx) = channel();
    drop(rx);
    let mut w = ChunkWriter::new(tx, |_| {});
    let e = w.write_all(&data(CHUNK_SIZE)).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn an_error_after_data_comes_on_the_next_read() {
    let (tx, rx) = channel();
    tx.send(Chunk::Data(vec![1, 2, 3])).unwrap();
    send_error(&tx, "worker restarted".into());
    let mut reader = ChunkReader::new(rx);
    let mut buf = [0; 10];
    assert_eq!(reader.read(&mut buf).unwrap(), 3);
    assert_eq!(reader.read(&mut buf).unwrap_err(), "worker restarted");
    assert!(reader.is_done());
}

#[test]
fn a_vanished_writer_is_an_error() {
    let (tx, rx) = channel();
    drop(tx);
    let mut reader = ChunkReader::new(rx);
    assert!(reader.read(&mut [0; 4]).is_err());
}

#[test]
fn pending_waits_for_the_value() {
    let slot = Arc::new(Pending::default());
    assert_eq!(slot.get(), None::<u32>);
    assert_eq!(slot.wait(Duration::from_millis(10)), None);
    let setter = Arc::clone(&slot);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        setter.set(7);
    });
    assert_eq!(slot.wait(Duration::from_secs(10)), Some(7));
    assert_eq!(slot.get(), Some(7));
}
