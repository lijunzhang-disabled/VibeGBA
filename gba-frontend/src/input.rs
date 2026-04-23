use gba_core::keypad::*;
use sdl2::keyboard::{KeyboardState, Scancode};

/// Read keyboard state and return a bitmask of pressed GBA buttons.
/// Returned as active-high (1 = pressed).
pub fn read_keyboard(keyboard: &KeyboardState) -> u16 {
    let mut keys = 0u16;

    if keyboard.is_scancode_pressed(Scancode::Z) { keys |= KEY_A; }
    if keyboard.is_scancode_pressed(Scancode::X) { keys |= KEY_B; }
    if keyboard.is_scancode_pressed(Scancode::Return) { keys |= KEY_START; }
    if keyboard.is_scancode_pressed(Scancode::Backspace) { keys |= KEY_SELECT; }
    if keyboard.is_scancode_pressed(Scancode::Right) { keys |= KEY_RIGHT; }
    if keyboard.is_scancode_pressed(Scancode::Left) { keys |= KEY_LEFT; }
    if keyboard.is_scancode_pressed(Scancode::Up) { keys |= KEY_UP; }
    if keyboard.is_scancode_pressed(Scancode::Down) { keys |= KEY_DOWN; }
    if keyboard.is_scancode_pressed(Scancode::A) { keys |= KEY_L; }
    if keyboard.is_scancode_pressed(Scancode::S) { keys |= KEY_R; }

    keys
}
