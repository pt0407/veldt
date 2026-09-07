// Parser for the Veldt language — recursive descent

use crate::ast::*;
use crate::lexer::Token;

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn check(&self, t: &Token) -> bool {
        self.peek() == t
    }

    fn match_tok(&mut self, t: &Token) -> bool {
        if self.check(t) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &Token, msg: &str) -> Result<Token, String> {
        if self.check(t) {
            Ok(self.advance())
        } else {
            Err(format!("Expected {} but got {:?} at pos {}", msg, self.peek(), self.pos))
        }
    }

    fn parse_type(&mut self) -> Result<Type, String> {
        match self.advance() {
            Token::TypeInt => Ok(Type::Int),
            Token::TypeStr => Ok(Type::Str),
            Token::TypeBool => Ok(Type::Bool),
            Token::TypeFn => Ok(Type::Fn),
            Token::TypeList => {
                // list<type> or just list
                if self.check(&Token::Lt) {
                    self.advance();
                    let inner = self.parse_type()?;
                    self.expect(&Token::Gt, ">")?;
                    Ok(Type::List(Box::new(inner)))
                } else {
                    Ok(Type::List(Box::new(Type::Int))) // default
                }
            }
            Token::Ident(name) => Ok(Type::Struct(name)),
            t => Err(format!("Expected type but got {:?}", t)),
        }
    }

    fn parse_param(&mut self) -> Result<Param, String> {
        let name = match self.advance() {
            Token::Ident(n) => n,
            Token::TypeInt => "int".into(),
            Token::TypeStr => "str".into(),
            Token::TypeBool => "bool".into(),
            Token::TypeList => "list".into(),
            Token::TypeFn => "fn".into(),
            t => return Err(format!("Expected param name but got {:?}", t)),
        };
        self.expect(&Token::Colon, ":")?;
        let typ = self.parse_type()?;
        Ok(Param { name, typ })
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, String> {
        let mut params = Vec::new();
        self.expect(&Token::LParen, "(")?;
        if !self.check(&Token::RParen) {
            loop {
                params.push(self.parse_param()?);
                if !self.match_tok(&Token::Comma) {
                    break;
                }
            }
        }
        self.expect(&Token::RParen, ")")?;
        Ok(params)
    }

    // Expression parsing with precedence climbing

    pub fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        while self.match_tok(&Token::Or) {
            let right = self.parse_and()?;
            left = Expr::BinOp(Box::new(left), BinOp::Or, Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_equality()?;
        while self.match_tok(&Token::And) {
            let right = self.parse_equality()?;
            left = Expr::BinOp(Box::new(left), BinOp::And, Box::new(right));
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_comparison()?;
        loop {
            let op = match self.peek() {
                Token::EqEq => BinOp::Eq,
                Token::Neq => BinOp::Neq,
                _ => break,
            };
            self.advance();
            let right = self.parse_comparison()?;
            left = Expr::BinOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_addition()?;
        loop {
            let op = match self.peek() {
                Token::Lt => BinOp::Lt,
                Token::Gt => BinOp::Gt,
                Token::Le => BinOp::Le,
                Token::Ge => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let right = self.parse_addition()?;
            left = Expr::BinOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_addition(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_multiplication()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplication()?;
            left = Expr::BinOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_multiplication(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::Percent => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = Expr::BinOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Token::Not => {
                self.advance();
                let e = self.parse_unary()?;
                Ok(Expr::UnaryOp(UnaryOp::Not, Box::new(e)))
            }
            Token::Minus => {
                self.advance();
                let e = self.parse_unary()?;
                Ok(Expr::UnaryOp(UnaryOp::Neg, Box::new(e)))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_ident_postfix(&mut self, name: String) -> Result<Expr, String> {
        // Check for variant marker: name#N
        let variant = if let Token::Hash(n) = self.peek() {
            let v = *n;
            self.advance();
            Some(v)
        } else {
            None
        };
        // Check for function call
        if self.check(&Token::LParen) {
            let mut args = Vec::new();
            self.advance(); // (
            if !self.check(&Token::RParen) {
                loop {
                    args.push(self.parse_expr()?);
                    if !self.match_tok(&Token::Comma) {
                        break;
                    }
                }
            }
            self.expect(&Token::RParen, ")")?;
            Ok(Expr::Call(name, variant, args))
        } else if self.check(&Token::LBrace) {
            // struct literal: Name { field: val, ... }
            self.advance(); // {
            let mut fields = Vec::new();
            if !self.check(&Token::RBrace) {
                loop {
                    let fname = match self.advance() {
                        Token::Ident(n) => n,
                        Token::TypeInt => "int".into(),
                        Token::TypeStr => "str".into(),
                        Token::TypeBool => "bool".into(),
                        Token::TypeList => "list".into(),
                        Token::TypeFn => "fn".into(),
                        t => return Err(format!("Expected field name but got {:?}", t)),
                    };
                    self.expect(&Token::Colon, ":")?;
                    let val = self.parse_expr()?;
                    fields.push((fname, val));
                    if !self.match_tok(&Token::Comma) {
                        break;
                    }
                }
            }
            self.expect(&Token::RBrace, "}")?;
            Ok(Expr::StructLit(name, fields))
        } else {
            Ok(Expr::Var(name))
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek() {
                Token::Dot => {
                    self.advance();
                    let method = match self.advance() {
                        Token::Ident(n) => n,
                        t => return Err(format!("Expected method/field name but got {:?}", t)),
                    };
                    if self.match_tok(&Token::LParen) {
                        // method call
                        let mut args = Vec::new();
                        if !self.check(&Token::RParen) {
                            loop {
                                args.push(self.parse_expr()?);
                                if !self.match_tok(&Token::Comma) {
                                    break;
                                }
                            }
                        }
                        self.expect(&Token::RParen, ")")?;
                        expr = Expr::MethodCall(Box::new(expr), method, args);
                    } else {
                        // field access
                        expr = Expr::FieldAccess(Box::new(expr), method);
                    }
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Token::Int(n) => {
                self.advance();
                Ok(Expr::Int(n))
            }
            Token::Str(s) => {
                self.advance();
                Ok(Expr::Str(s))
            }
            Token::True => {
                self.advance();
                Ok(Expr::Bool(true))
            }
            Token::False => {
                self.advance();
                Ok(Expr::Bool(false))
            }
            Token::LParen => {
                self.advance();
                let e = self.parse_expr()?;
                self.expect(&Token::RParen, ")")?;
                Ok(e)
            }
            Token::LBracket => {
                self.advance();
                let mut items = Vec::new();
                if !self.check(&Token::RBracket) {
                    loop {
                        items.push(self.parse_expr()?);
                        if !self.match_tok(&Token::Comma) {
                            break;
                        }
                    }
                }
                self.expect(&Token::RBracket, "]")?;
                Ok(Expr::List(items))
            }
            Token::Ident(name) => {
                self.advance();
                self.parse_ident_postfix(name)
            }
            Token::TypeInt => {
                self.advance();
                self.parse_ident_postfix("int".into())
            }
            Token::TypeStr => {
                self.advance();
                self.parse_ident_postfix("str".into())
            }
            Token::TypeBool => {
                self.advance();
                self.parse_ident_postfix("bool".into())
            }
            Token::TypeList => {
                self.advance();
                self.parse_ident_postfix("list".into())
            }
            Token::TypeFn => {
                self.advance();
                self.parse_ident_postfix("fn".into())
            }
            t => Err(format!("Unexpected token in expression: {:?}", t)),
        }
    }

    // Statement parsing

    pub fn parse_program(&mut self) -> Result<Vec<Stmt>, String> {
        let mut stmts = Vec::new();
        while !self.check(&Token::Eof) {
            stmts.push(self.parse_stmt()?);
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        match self.peek().clone() {
            Token::Let => self.parse_let(),
            Token::Fn => self.parse_fn(),
            Token::If => self.parse_if(),
            Token::While => self.parse_while(),
            Token::For => self.parse_for(),
            Token::Return => self.parse_return(),
            Token::Print => self.parse_print(),
            Token::Grow => self.parse_grow(),
            Token::Struct => self.parse_struct(),
            Token::Ident(name) => {
                // could be assignment or expression statement
                if self.tokens.get(self.pos + 1) == Some(&Token::Eq) {
                    self.advance(); // ident
                    self.advance(); // =
                    let expr = self.parse_expr()?;
                    Ok(Stmt::Assign(name, expr))
                } else {
                    let e = self.parse_expr()?;
                    Ok(Stmt::ExprStmt(e))
                }
            }
            t => Err(format!("Unexpected token in statement: {:?}", t)),
        }
    }

    fn parse_let(&mut self) -> Result<Stmt, String> {
        self.advance(); // let
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => return Err(format!("Expected variable name but got {:?}", t)),
        };
        self.expect(&Token::Eq, "=")?;
        let expr = self.parse_expr()?;
        Ok(Stmt::Let(name, expr))
    }

    fn parse_fn(&mut self) -> Result<Stmt, String> {
        self.advance(); // fn
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => return Err(format!("Expected function name but got {:?}", t)),
        };
        let params = self.parse_params()?;
        let body = self.parse_block()?;
        Ok(Stmt::FnDef(name, params, body))
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>, String> {
        self.expect(&Token::LBrace, "{")?;
        let mut stmts = Vec::new();
        while !self.check(&Token::RBrace) && !self.check(&Token::Eof) {
            stmts.push(self.parse_stmt()?);
        }
        self.expect(&Token::RBrace, "}")?;
        Ok(stmts)
    }

    fn parse_if(&mut self) -> Result<Stmt, String> {
        self.advance(); // if
        let cond = self.parse_expr()?;
        let then_block = self.parse_block()?;
        let else_block = if self.match_tok(&Token::Else) {
            if self.check(&Token::If) {
                Some(vec![self.parse_if()?])
            } else {
                Some(self.parse_block()?)
            }
        } else {
            None
        };
        Ok(Stmt::If(cond, then_block, else_block))
    }

    fn parse_while(&mut self) -> Result<Stmt, String> {
        self.advance(); // while
        let cond = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(Stmt::While(cond, body))
    }

    fn parse_for(&mut self) -> Result<Stmt, String> {
        self.advance(); // for
        let var = match self.advance() {
            Token::Ident(n) => n,
            t => return Err(format!("Expected loop variable but got {:?}", t)),
        };
        // expect "in"
        match self.advance() {
            Token::Ident(n) if n == "in" => {}
            t => return Err(format!("Expected 'in' but got {:?}", t)),
        }
        let iter = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(Stmt::For(var, iter, body))
    }

    fn parse_return(&mut self) -> Result<Stmt, String> {
        self.advance(); // return
        // return can be bare or with expression
        if self.check(&Token::RBrace) || self.check(&Token::Eof) {
            Ok(Stmt::Return(None))
        } else {
            let e = self.parse_expr()?;
            Ok(Stmt::Return(Some(e)))
        }
    }

    fn parse_print(&mut self) -> Result<Stmt, String> {
        self.advance(); // print
        self.expect(&Token::LParen, "(")?;
        let e = self.parse_expr()?;
        self.expect(&Token::RParen, ")")?;
        Ok(Stmt::Print(e))
    }

    fn parse_struct(&mut self) -> Result<Stmt, String> {
        self.advance(); // struct
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => return Err(format!("Expected struct name but got {:?}", t)),
        };
        self.expect(&Token::LBrace, "{")?;
        let mut fields = Vec::new();
        while !self.check(&Token::RBrace) {
            let fname = match self.advance() {
                Token::Ident(n) => n,
                t => return Err(format!("Expected field name but got {:?}", t)),
            };
            self.expect(&Token::Colon, ":")?;
            let ftype = self.parse_type()?;
            fields.push((fname, ftype));
            self.match_tok(&Token::Comma); // optional trailing comma
        }
        self.expect(&Token::RBrace, "}")?;
        Ok(Stmt::StructDef(name, fields))
    }

    fn parse_grow(&mut self) -> Result<Stmt, String> {
        self.advance(); // grow
        match self.peek().clone() {
            Token::Fn => {
                self.advance(); // fn
                let name = match self.advance() {
                    Token::Ident(n) => n,
                    t => return Err(format!("Expected function name but got {:?}", t)),
                };
                let params = self.parse_params()?;
                // optional test assertions
                let mut tests = Vec::new();
                if self.match_tok(&Token::Test) {
                    // test expr == expr, expr == expr, ... { body }
                    // Parse each assertion as a full expression, then decompose at ==
                    loop {
                        let expr = self.parse_expr()?;
                        match expr {
                            Expr::BinOp(call, BinOp::Eq, expected) => {
                                tests.push(TestAssert { call: *call, expected: *expected });
                            }
                            _ => return Err("Test assertion must be of form: expr == expr".into()),
                        }
                        if !self.match_tok(&Token::Comma) {
                            break;
                        }
                    }
                }
                let body = self.parse_block()?;
                Ok(Stmt::Grow(GrowItem::Fn(name, params, body, tests)))
            }
            Token::Let => {
                self.advance(); // let
                let name = match self.advance() {
                    Token::Ident(n) => n,
                    t => return Err(format!("Expected variable name but got {:?}", t)),
                };
                self.expect(&Token::Eq, "=")?;
                let expr = self.parse_expr()?;
                Ok(Stmt::Grow(GrowItem::Let(name, expr)))
            }
            Token::Struct => {
                self.advance(); // struct
                let name = match self.advance() {
                    Token::Ident(n) => n,
                    t => return Err(format!("Expected struct name but got {:?}", t)),
                };
                self.expect(&Token::LBrace, "{")?;
                let mut fields = Vec::new();
                while !self.check(&Token::RBrace) {
                    let fname = match self.advance() {
                        Token::Ident(n) => n,
                        t => return Err(format!("Expected field name but got {:?}", t)),
                    };
                    self.expect(&Token::Colon, ":")?;
                    let ftype = self.parse_type()?;
                    fields.push((fname, ftype));
                    self.match_tok(&Token::Comma);
                }
                self.expect(&Token::RBrace, "}")?;
                Ok(Stmt::Grow(GrowItem::Struct(name, fields)))
            }
            t => Err(format!("Expected fn, let, or struct after grow but got {:?}", t)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse(src: &str) -> Vec<Stmt> {
        let mut lex = Lexer::new(src);
        let tokens = lex.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        parser.parse_program().unwrap()
    }

    #[test]
    fn test_let() {
        let stmts = parse("let x = 5");
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], Stmt::Let("x".into(), Expr::Int(5)));
    }

    #[test]
    fn test_fn_def() {
        let stmts = parse("fn add(a: int, b: int) { return a + b }");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Stmt::FnDef(name, params, body) => {
                assert_eq!(name, "add");
                assert_eq!(params.len(), 2);
                assert_eq!(body.len(), 1);
            }
            s => panic!("Expected FnDef but got {:?}", s),
        }
    }

    #[test]
    fn test_if_else() {
        let stmts = parse("if x > 3 { print(x) } else { print(0) }");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Stmt::If(_, then_block, Some(else_block)) => {
                assert_eq!(then_block.len(), 1);
                assert_eq!(else_block.len(), 1);
            }
            s => panic!("Expected If with else but got {:?}", s),
        }
    }

    #[test]
    fn test_for_loop() {
        let stmts = parse("for i in range(10) { print(i) }");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Stmt::For(var, _, body) => {
                assert_eq!(var, "i");
                assert_eq!(body.len(), 1);
            }
            s => panic!("Expected For but got {:?}", s),
        }
    }

    #[test]
    fn test_grow_fn() {
        let stmts = parse("grow fn shout(name: str) { print(name) }");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Stmt::Grow(GrowItem::Fn(name, params, _, tests)) => {
                assert_eq!(name, "shout");
                assert_eq!(params.len(), 1);
                assert!(tests.is_empty());
            }
            s => panic!("Expected Grow Fn but got {:?}", s),
        }
    }

    #[test]
    fn test_grow_fn_with_test() {
        let stmts = parse("grow fn sort(list: list) test sort([3, 1, 2]) == [1, 2, 3] { return list }");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Stmt::Grow(GrowItem::Fn(name, _, _, tests)) => {
                assert_eq!(name, "sort");
                assert_eq!(tests.len(), 1);
            }
            s => panic!("Expected Grow Fn with test but got {:?}", s),
        }
    }

    #[test]
    fn test_variant_call() {
        let stmts = parse("sort#2(my_list)");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Stmt::ExprStmt(Expr::Call(name, variant, args)) => {
                assert_eq!(name, "sort");
                assert_eq!(*variant, Some(2));
                assert_eq!(args.len(), 1);
            }
            s => panic!("Expected variant call but got {:?}", s),
        }
    }

    #[test]
    fn test_struct_def() {
        let stmts = parse("struct Point { x: int, y: int }");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Stmt::StructDef(name, fields) => {
                assert_eq!(name, "Point");
                assert_eq!(fields.len(), 2);
            }
            s => panic!("Expected StructDef but got {:?}", s),
        }
    }

    #[test]
    fn test_struct_literal() {
        let stmts = parse("let p = Point { x: 3, y: 4 }");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Stmt::Let(name, Expr::StructLit(sname, fields)) => {
                assert_eq!(name, "p");
                assert_eq!(sname, "Point");
                assert_eq!(fields.len(), 2);
            }
            s => panic!("Expected struct literal but got {:?}", s),
        }
    }

    #[test]
    fn test_method_call() {
        let stmts = parse("x.upper()");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Stmt::ExprStmt(Expr::MethodCall(obj, method, args)) => {
                assert_eq!(method, "upper");
                assert_eq!(args.len(), 0);
            }
            s => panic!("Expected method call but got {:?}", s),
        }
    }

    #[test]
    fn test_field_access() {
        let stmts = parse("print(p.x)");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Stmt::Print(Expr::FieldAccess(obj, field)) => {
                assert_eq!(field, "x");
            }
            s => panic!("Expected field access but got {:?}", s),
        }
    }

    #[test]
    fn test_assignment() {
        let stmts = parse("x = 10");
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], Stmt::Assign("x".into(), Expr::Int(10)));
    }

    #[test]
    fn test_operator_precedence() {
        let stmts = parse("let x = 1 + 2 * 3");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Stmt::Let(_, Expr::BinOp(_, BinOp::Add, right)) => {
                // right should be 2 * 3
                match right.as_ref() {
                    Expr::BinOp(_, BinOp::Mul, _) => {}
                    e => panic!("Expected Mul but got {:?}", e),
                }
            }
            s => panic!("Expected Add at top level but got {:?}", s),
        }
    }
}
