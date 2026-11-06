//! Guards the stack requirement measured for the vendored rsleigh decoder.
//!
//! Decoding `call [IAT]` in a debug build overflows the default 2 MiB test
//! thread stack and succeeds from 8 MiB. The library therefore exposes
//! [`wlam_sleigh::with_big_stack`]; this test is the regression guard that the
//! helper actually works.
use wlam_sleigh::{lift_block, with_big_stack, IR, LIFT_STACK_SIZE};

#[test]
fn lifting_succeeds_on_the_library_thread() {
    assert!(
        LIFT_STACK_SIZE >= 8 * 1024 * 1024,
        "measured requirement is >2 MiB, safe from 8 MiB"
    );
    let block = with_big_stack(|| lift_block(&[0xff, 0x15, 0x6c, 0x10, 0x41, 0x00], 0x40ccd8));
    assert_eq!(block.instructions.len(), 1);
    assert!(block.instructions[0]
        .ops
        .iter()
        .any(|o| matches!(o, IR::CallInd { .. })));
}

#[test]
fn panic_inside_lifting_is_propagated_not_swallowed() {
    let r = std::panic::catch_unwind(|| with_big_stack(|| -> u8 { panic!("boom") }));
    assert!(
        r.is_err(),
        "a panic in the lifting thread must resume on the caller"
    );
}
