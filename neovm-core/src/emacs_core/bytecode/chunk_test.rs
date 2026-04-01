use super::*;

#[test]
fn constant_dedup() {
    crate::test_utils::init_test_tracing();
    let mut func = ByteCodeFunction::new(LambdaParams::simple(vec![]));
    let i1 = func.add_constant(Value::fixnum(42));
    let i2 = func.add_constant(Value::fixnum(42));
    assert_eq!(i1, i2);
    assert_eq!(func.constants.len(), 1);
}

#[test]
fn symbol_dedup() {
    crate::test_utils::init_test_tracing();
    let mut func = ByteCodeFunction::new(LambdaParams::simple(vec![]));
    let i1 = func.add_symbol("x");
    let i2 = func.add_symbol("x");
    let i3 = func.add_symbol("y");
    assert_eq!(i1, i2);
    assert_ne!(i1, i3);
    assert_eq!(func.constants.len(), 2);
}

#[test]
fn patch_jump() {
    crate::test_utils::init_test_tracing();
    let mut func = ByteCodeFunction::new(LambdaParams::simple(vec![]));
    func.emit(Op::GotoIfNil(0)); // placeholder
    func.emit(Op::Constant(0));
    func.emit(Op::Return);
    let target = func.current_offset();
    func.patch_jump(0, target);
    assert_eq!(func.ops[0], Op::GotoIfNil(3));
}

#[test]
fn disassemble_output() {
    crate::test_utils::init_test_tracing();
    let mut func = ByteCodeFunction::new(LambdaParams::simple(vec![]));
    func.add_constant(Value::fixnum(42));
    func.emit(Op::Constant(0));
    func.emit(Op::Return);
    let dis = func.disassemble();
    assert!(dis.contains("constant 0 ; 42"));
    assert!(dis.contains("return"));
}
