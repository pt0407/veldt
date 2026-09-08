// AST type definitions for the Veldt language

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Type {
    Int,
    Float,
    Str,
    Bool,
    List(Box<Type>),
    Fn,
    Struct(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub typ: Type,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BinOp {
    Add, Sub, Mul, Div, Mod,
    Eq, Neq, Lt, Gt, Le, Ge,
    And, Or,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UnaryOp {
    Neg, Not,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    List(Vec<Expr>),
    Var(String),
    BinOp(Box<Expr>, BinOp, Box<Expr>),
    UnaryOp(UnaryOp, Box<Expr>),
    // function call: name, optional variant number (sort#2), args
    Call(String, Option<u32>, Vec<Expr>),
    // method call: obj.method(args)
    MethodCall(Box<Expr>, String, Vec<Expr>),
    // field access: obj.field
    FieldAccess(Box<Expr>, String),
    // struct literal: Name { field: val, ... }
    StructLit(String, Vec<(String, Expr)>),
    // list indexing: expr[index]
    Index(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestAssert {
    pub call: Expr,
    pub expected: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Stmt {
    Let(String, Expr),
    Assign(String, Expr),
    // obj.field = expr
    FieldAssign(Expr, String, Expr),
    // obj[index] = expr
    IndexAssign(Expr, Expr, Expr),
    FnDef(String, Vec<Param>, Vec<Stmt>),
    StructDef(String, Vec<(String, Type)>),
    If(Expr, Vec<Stmt>, Option<Vec<Stmt>>),
    While(Expr, Vec<Stmt>),
    For(String, Expr, Vec<Stmt>),
    Return(Option<Expr>),
    ExprStmt(Expr),
    Print(Expr),
    Grow(GrowItem),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GrowItem {
    Fn(String, Vec<Param>, Vec<Stmt>, Vec<TestAssert>),
    Let(String, Expr),
    Struct(String, Vec<(String, Type)>),
}
