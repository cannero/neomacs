use super::*;

#[test]
fn test_portable_pty_explicit_cmd() {
    use std::io::Read;

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("create pty");
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", "echo PORTABLE_PTY_OK; sleep 1"]);
    let mut child = pair.slave.spawn_command(cmd).expect("spawn child");
    let mut reader = pair.master.try_clone_reader().expect("clone");
    let mut buf = [0u8; 4096];
    std::thread::sleep(std::time::Duration::from_millis(500));

    match reader.read(&mut buf) {
        Ok(n) if n > 0 => {
            let output = String::from_utf8_lossy(&buf[..n]);
            assert!(output.contains("PORTABLE_PTY_OK"));
        }
        Ok(_) => panic!("EOF"),
        Err(e) => panic!("Read error: {}", e),
    }

    let _ = child.wait();
}
