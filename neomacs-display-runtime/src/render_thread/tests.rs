use super::*;
use crate::thread_comm::ThreadComms;
use winit::keyboard::{Key, NamedKey};

#[test]
fn test_translate_key_named() {
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::Escape)),
        0xff1b
    );
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::Enter)),
        0xff0d
    );
    assert_eq!(RenderApp::translate_key(&Key::Named(NamedKey::Tab)), 0xff09);
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::Backspace)),
        0xff08
    );
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::Delete)),
        0xffff
    );
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::Home)),
        0xff50
    );
    assert_eq!(RenderApp::translate_key(&Key::Named(NamedKey::End)), 0xff57);
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::PageUp)),
        0xff55
    );
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::PageDown)),
        0xff56
    );
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::ArrowLeft)),
        0xff51
    );
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::ArrowUp)),
        0xff52
    );
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::ArrowRight)),
        0xff53
    );
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::ArrowDown)),
        0xff54
    );
    assert_eq!(RenderApp::translate_key(&Key::Named(NamedKey::Space)), 0x20);
}

#[test]
fn test_translate_key_character() {
    assert_eq!(
        RenderApp::translate_key(&Key::Character("a".into())),
        'a' as u32
    );
    assert_eq!(
        RenderApp::translate_key(&Key::Character("A".into())),
        'A' as u32
    );
    assert_eq!(
        RenderApp::translate_key(&Key::Character("1".into())),
        '1' as u32
    );
}

#[test]
fn test_translate_key_function_keys() {
    assert_eq!(RenderApp::translate_key(&Key::Named(NamedKey::F1)), 0xffbe);
    assert_eq!(RenderApp::translate_key(&Key::Named(NamedKey::F12)), 0xffc9);
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::Insert)),
        0xff63
    );
    assert_eq!(
        RenderApp::translate_key(&Key::Named(NamedKey::PrintScreen)),
        0xff61
    );
}

#[test]
fn test_translate_key_unknown() {
    assert_eq!(RenderApp::translate_key(&Key::Dead(None)), 0);
}

#[test]
fn test_render_thread_creation() {
    let comms = ThreadComms::new().expect("Failed to create ThreadComms");
    let (emacs, render) = comms.split();

    assert!(emacs.input_rx.is_empty());
    assert!(render.cmd_rx.is_empty());
}
