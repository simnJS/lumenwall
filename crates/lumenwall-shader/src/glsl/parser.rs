//! Recursive-descent GLSL parser over preprocessed tokens.

use std::collections::HashSet;

use super::ast::*;
use super::lexer::{P, TokKind, Token};
use super::types::{Interner, Kw, Name, Type, keyword_type};

#[derive(Debug, Clone)]
pub struct ParseError {
    pub line: u32,
    pub msg: String,
}

type PResult<T> = Result<T, ParseError>;

pub struct Parser<'a> {
    toks: &'a [Token],
    pos: usize,
    int: &'a Interner,
    struct_names: HashSet<Name>,
    unit: Unit,
}

pub fn parse(toks: &[Token], int: &Interner) -> PResult<Unit> {
    let mut p = Parser {
        toks,
        pos: 0,
        int,
        struct_names: HashSet::new(),
        unit: Unit::default(),
    };
    while p.pos < p.toks.len() {
        if p.eat(P::Semicolon) {
            continue;
        }
        let item = p.external()?;
        p.unit.items.push(item);
    }
    Ok(p.unit)
}

fn is_qualifier_kw(k: Kw) -> bool {
    use Kw::*;
    matches!(
        k,
        Const
            | In
            | Out
            | Inout
            | Uniform
            | Attribute
            | Varying
            | Flat
            | Smooth
            | Noperspective
            | Centroid
            | Invariant
            | Precise
            | Highp
            | Mediump
            | Lowp
            | Layout
            | Buffer
            | Shared
    )
}

impl<'a> Parser<'a> {
    fn line(&self) -> u32 {
        self.toks
            .get(self.pos)
            .or(self.toks.last())
            .map_or(0, |t| t.line)
    }
    fn err<T>(&self, msg: impl Into<String>) -> PResult<T> {
        Err(ParseError {
            line: self.line(),
            msg: msg.into(),
        })
    }
    fn peek(&self) -> Option<TokKind> {
        self.toks.get(self.pos).map(|t| t.kind)
    }
    fn peek_at(&self, n: usize) -> Option<TokKind> {
        self.toks.get(self.pos + n).map(|t| t.kind)
    }
    fn is(&self, p: P) -> bool {
        self.peek() == Some(TokKind::Punct(p))
    }
    fn eat(&mut self, p: P) -> bool {
        if self.is(p) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, p: P) -> PResult<()> {
        if self.eat(p) {
            Ok(())
        } else {
            let found = self.describe_current();
            self.err(format!("expected '{}', found {}", p.text(), found))
        }
    }
    fn describe_current(&self) -> String {
        match self.peek() {
            None => "end of input".into(),
            Some(TokKind::Ident(n)) | Some(TokKind::Number(n)) => format!("'{}'", self.int.get(n)),
            Some(TokKind::Punct(p)) => format!("'{}'", p.text()),
            Some(TokKind::Other(c)) => format!("'{c}'"),
            Some(TokKind::Newline) => "newline".into(),
        }
    }
    fn kw_at(&self, n: usize) -> Option<Kw> {
        match self.peek_at(n) {
            Some(TokKind::Ident(id)) => Kw::from_id(id),
            _ => None,
        }
    }
    fn kw(&self) -> Option<Kw> {
        self.kw_at(0)
    }
    fn eat_kw(&mut self, k: Kw) -> bool {
        if self.kw() == Some(k) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn ident(&mut self) -> PResult<Name> {
        match self.peek() {
            Some(TokKind::Ident(n)) if Kw::from_id(n).is_none() => {
                self.pos += 1;
                Ok(n)
            }
            _ => {
                let f = self.describe_current();
                self.err(format!("expected identifier, found {f}"))
            }
        }
    }
    /// True if the token at offset `n` starts a type.
    fn is_type_at(&self, n: usize) -> bool {
        match self.peek_at(n) {
            Some(TokKind::Ident(id)) => match Kw::from_id(id) {
                Some(Kw::Struct) => true,
                Some(k) => keyword_type(k).is_some() || k == Kw::Double,
                None => self.struct_names.contains(&id),
            },
            _ => false,
        }
    }

    // ---- declarations ----------------------------------------------------

    fn qualifiers(&mut self) -> PResult<(Qualifiers, bool)> {
        let mut q = Qualifiers::default();
        let mut any = false;
        while let Some(k) = self.kw() {
            if !is_qualifier_kw(k) {
                break;
            }
            any = true;
            self.pos += 1;
            match k {
                Kw::Const => {
                    if q.storage == Storage::None {
                        q.storage = Storage::Const;
                    }
                    q.is_const = true;
                }
                Kw::In => {
                    q.storage = if q.storage == Storage::Out {
                        Storage::InOut
                    } else {
                        Storage::In
                    }
                }
                Kw::Out => {
                    q.storage = if q.storage == Storage::In {
                        Storage::InOut
                    } else {
                        Storage::Out
                    }
                }
                Kw::Inout => q.storage = Storage::InOut,
                Kw::Uniform => q.storage = Storage::Uniform,
                Kw::Attribute => q.storage = Storage::Attribute,
                Kw::Varying => q.storage = Storage::Varying,
                Kw::Buffer => q.storage = Storage::Buffer,
                Kw::Shared => q.storage = Storage::Shared,
                Kw::Flat => q.interp = Interp::Flat,
                Kw::Smooth => q.interp = Interp::Smooth,
                Kw::Noperspective => q.interp = Interp::NoPerspective,
                Kw::Centroid => q.centroid = true,
                Kw::Invariant => q.invariant = true,
                Kw::Layout => {
                    q.layout = true;
                    self.expect(P::LParen)?;
                    let mut depth = 1;
                    while depth > 0 {
                        match self.peek() {
                            None => return self.err("unterminated layout qualifier"),
                            Some(TokKind::Punct(P::LParen)) => depth += 1,
                            Some(TokKind::Punct(P::RParen)) => depth -= 1,
                            _ => {}
                        }
                        self.pos += 1;
                    }
                }
                _ => {} // precision qualifiers, precise: ignored
            }
        }
        // `const in` parameters keep storage In with `is_const` set.
        Ok((q, any))
    }

    fn array_spec(&mut self) -> PResult<ArraySpec> {
        if !self.eat(P::LBracket) {
            return Ok(None);
        }
        if self.eat(P::RBracket) {
            return Ok(Some(None));
        }
        let e = self.expr()?;
        self.expect(P::RBracket)?;
        if self.is(P::LBracket) {
            return self.err("arrays of arrays are not supported");
        }
        Ok(Some(Some(Box::new(e))))
    }

    fn full_type(&mut self) -> PResult<FullType> {
        let line = self.line();
        let spec = match self.peek() {
            Some(TokKind::Ident(id)) => match Kw::from_id(id) {
                Some(Kw::Struct) => {
                    self.pos += 1;
                    TypeSpec::StructDef(self.struct_def(line)?)
                }
                Some(Kw::Double) => return self.err("'double' is not supported"),
                Some(k) => match keyword_type(k) {
                    Some(t) => {
                        self.pos += 1;
                        TypeSpec::Builtin(t)
                    }
                    None => {
                        return self.err(format!("expected a type, found '{}'", self.int.get(id)));
                    }
                },
                None if self.struct_names.contains(&id) => {
                    self.pos += 1;
                    TypeSpec::Named(id)
                }
                None => return self.err(format!("unknown type '{}'", self.int.get(id))),
            },
            _ => {
                let f = self.describe_current();
                return self.err(format!("expected a type, found {f}"));
            }
        };
        let array = self.array_spec()?;
        Ok(FullType { spec, array, line })
    }

    fn struct_def(&mut self, line: u32) -> PResult<u32> {
        let name = if self.is(P::LBrace) {
            None
        } else {
            Some(self.ident()?)
        };
        if let Some(n) = name {
            self.struct_names.insert(n);
        }
        self.expect(P::LBrace)?;
        let mut fields = Vec::new();
        while !self.eat(P::RBrace) {
            let _ = self.qualifiers()?;
            let ty = self.full_type()?;
            let mut names = Vec::new();
            loop {
                let n = self.ident()?;
                let a = self.array_spec()?;
                names.push((n, a));
                if !self.eat(P::Comma) {
                    break;
                }
            }
            self.expect(P::Semicolon)?;
            fields.push((ty, names));
        }
        self.unit.structs.push(StructDefAst {
            name,
            fields,
            line,
            resolved: None,
        });
        Ok(self.unit.structs.len() as u32 - 1)
    }

    fn external(&mut self) -> PResult<Item> {
        let line = self.line();
        if self.eat_kw(Kw::Precision) {
            while !self.eat(P::Semicolon) {
                if self.peek().is_none() {
                    return self.err("unterminated precision statement");
                }
                self.pos += 1;
            }
            return Ok(Item::Ignored);
        }
        let (quals, any_q) = self.qualifiers()?;
        if any_q {
            if self.eat(P::Semicolon) {
                return Ok(Item::Ignored);
            }
            if let Some(TokKind::Ident(id)) = self.peek()
                && Kw::from_id(id).is_none()
                && !self.struct_names.contains(&id)
            {
                match self.peek_at(1) {
                    Some(TokKind::Punct(P::LBrace)) => {
                        return self.err("interface blocks are not supported");
                    }
                    Some(TokKind::Punct(P::Semicolon)) | Some(TokKind::Punct(P::Comma)) => {
                        while !self.eat(P::Semicolon) {
                            if self.peek().is_none() {
                                break;
                            }
                            self.pos += 1;
                        }
                        return Ok(Item::Ignored);
                    }
                    _ => {}
                }
            }
        }
        let ty = self.full_type()?;
        if self.eat(P::Semicolon) {
            return Ok(Item::Decl(Decl {
                quals,
                ty,
                vars: Vec::new(),
                line,
            }));
        }
        let name = self.ident()?;
        if self.is(P::LParen) {
            return self.function(ty, name, line).map(Item::Function);
        }
        let vars = self.declarators(name)?;
        Ok(Item::Decl(Decl {
            quals,
            ty,
            vars,
            line,
        }))
    }

    fn declarators(&mut self, first: Name) -> PResult<Vec<Declarator>> {
        let mut vars = Vec::new();
        let mut name = first;
        loop {
            let line = self.line();
            let array = self.array_spec()?;
            let init = if self.eat(P::Assign) {
                if self.is(P::LBrace) {
                    return self.err("initializer lists are not supported");
                }
                Some(self.assign()?)
            } else {
                None
            };
            vars.push(Declarator {
                name,
                array,
                init,
                line,
                ty: Type::Void,
                sym: Sym::Unresolved,
            });
            if self.eat(P::Comma) {
                name = self.ident()?;
                continue;
            }
            self.expect(P::Semicolon)?;
            return Ok(vars);
        }
    }

    fn function(&mut self, ret: FullType, name: Name, line: u32) -> PResult<FunctionDef> {
        self.expect(P::LParen)?;
        let mut params = Vec::new();
        if self.kw() == Some(Kw::Void) && self.peek_at(1) == Some(TokKind::Punct(P::RParen)) {
            self.pos += 1;
        }
        if !self.eat(P::RParen) {
            loop {
                let (q, _) = self.qualifiers()?;
                let dir = match q.storage {
                    Storage::Out => ParamDir::Out,
                    Storage::InOut => ParamDir::InOut,
                    _ => ParamDir::In,
                };
                let ty = self.full_type()?;
                let (pname, array) = if let Some(TokKind::Ident(id)) = self.peek()
                    && Kw::from_id(id).is_none()
                {
                    self.pos += 1;
                    (Some(id), self.array_spec()?)
                } else {
                    (None, None)
                };
                params.push(Param {
                    dir,
                    is_const: q.is_const,
                    ty,
                    name: pname,
                    array,
                    rty: Type::Void,
                    local: 0,
                });
                if self.eat(P::Comma) {
                    continue;
                }
                self.expect(P::RParen)?;
                break;
            }
        }
        let body = if self.eat(P::Semicolon) {
            None
        } else {
            self.expect(P::LBrace)?;
            Some(self.block_body()?)
        };
        Ok(FunctionDef {
            ret,
            name,
            params,
            body,
            line,
            fn_index: 0,
            skip: false,
        })
    }

    // ---- statements ------------------------------------------------------

    fn block_body(&mut self) -> PResult<Vec<Stmt>> {
        let mut v = Vec::new();
        while !self.eat(P::RBrace) {
            if self.peek().is_none() {
                return self.err("unexpected end of input, expected '}'");
            }
            v.push(self.stmt()?);
        }
        Ok(v)
    }

    fn is_decl_start(&self) -> bool {
        match self.kw() {
            Some(Kw::Precision) => return true,
            Some(k) if is_qualifier_kw(k) => return true,
            Some(Kw::Struct) => return true,
            _ => {}
        }
        if !self.is_type_at(0) {
            return false;
        }
        match self.peek_at(1) {
            Some(TokKind::Punct(P::LParen)) => false,
            Some(TokKind::Punct(P::LBracket)) => {
                // `float[3] a` (declaration) vs `float[3](...)` (constructor).
                let mut i = 2;
                let mut depth = 1;
                while let Some(k) = self.peek_at(i) {
                    match k {
                        TokKind::Punct(P::LBracket) => depth += 1,
                        TokKind::Punct(P::RBracket) => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                self.peek_at(i + 1) != Some(TokKind::Punct(P::LParen))
            }
            Some(TokKind::Punct(P::Dot)) => false,
            _ => true,
        }
    }

    fn local_decl(&mut self) -> PResult<Stmt> {
        let line = self.line();
        if self.eat_kw(Kw::Precision) {
            while !self.eat(P::Semicolon) {
                if self.peek().is_none() {
                    return self.err("unterminated precision statement");
                }
                self.pos += 1;
            }
            return Ok(Stmt::Empty);
        }
        let (quals, _) = self.qualifiers()?;
        let ty = self.full_type()?;
        if self.eat(P::Semicolon) {
            return Ok(Stmt::Decl(Decl {
                quals,
                ty,
                vars: Vec::new(),
                line,
            }));
        }
        let name = self.ident()?;
        let vars = self.declarators(name)?;
        Ok(Stmt::Decl(Decl {
            quals,
            ty,
            vars,
            line,
        }))
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let line = self.line();
        if self.eat(P::LBrace) {
            return Ok(Stmt::Block(self.block_body()?));
        }
        if self.eat(P::Semicolon) {
            return Ok(Stmt::Empty);
        }
        match self.kw() {
            Some(Kw::If) => {
                self.pos += 1;
                self.expect(P::LParen)?;
                let c = self.expr()?;
                self.expect(P::RParen)?;
                let t = self.stmt()?;
                let e = if self.eat_kw(Kw::Else) {
                    Some(Box::new(self.stmt()?))
                } else {
                    None
                };
                return Ok(Stmt::If(c, Box::new(t), e));
            }
            Some(Kw::Switch) => {
                self.pos += 1;
                self.expect(P::LParen)?;
                let c = self.expr()?;
                self.expect(P::RParen)?;
                self.expect(P::LBrace)?;
                let body = self.block_body()?;
                return Ok(Stmt::Switch(c, body));
            }
            Some(Kw::Case) => {
                self.pos += 1;
                let e = self.expr()?;
                self.expect(P::Colon)?;
                return Ok(Stmt::Case(e));
            }
            Some(Kw::Default) => {
                self.pos += 1;
                self.expect(P::Colon)?;
                return Ok(Stmt::Default);
            }
            Some(Kw::While) => {
                self.pos += 1;
                self.expect(P::LParen)?;
                let c = self.expr()?;
                self.expect(P::RParen)?;
                let b = self.stmt()?;
                return Ok(Stmt::While(c, Box::new(b)));
            }
            Some(Kw::Do) => {
                self.pos += 1;
                let b = self.stmt()?;
                if !self.eat_kw(Kw::While) {
                    return self.err("expected 'while' after 'do' body");
                }
                self.expect(P::LParen)?;
                let c = self.expr()?;
                self.expect(P::RParen)?;
                self.expect(P::Semicolon)?;
                return Ok(Stmt::DoWhile(Box::new(b), c));
            }
            Some(Kw::For) => {
                self.pos += 1;
                self.expect(P::LParen)?;
                let init = if self.eat(P::Semicolon) {
                    None
                } else if self.is_decl_start() {
                    Some(Box::new(self.local_decl()?))
                } else {
                    let e = self.expr()?;
                    self.expect(P::Semicolon)?;
                    Some(Box::new(Stmt::Expr(e)))
                };
                let cond = if self.is(P::Semicolon) {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.expect(P::Semicolon)?;
                let step = if self.is(P::RParen) {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.expect(P::RParen)?;
                let body = Box::new(self.stmt()?);
                return Ok(Stmt::For {
                    init,
                    cond,
                    step,
                    body,
                });
            }
            Some(Kw::Break) => {
                self.pos += 1;
                self.expect(P::Semicolon)?;
                return Ok(Stmt::Break);
            }
            Some(Kw::Continue) => {
                self.pos += 1;
                self.expect(P::Semicolon)?;
                return Ok(Stmt::Continue);
            }
            Some(Kw::Discard) => {
                self.pos += 1;
                self.expect(P::Semicolon)?;
                return Ok(Stmt::Discard);
            }
            Some(Kw::Return) => {
                self.pos += 1;
                let e = if self.is(P::Semicolon) {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.expect(P::Semicolon)?;
                return Ok(Stmt::Return(e, line));
            }
            _ => {}
        }
        if self.is_decl_start() {
            return self.local_decl();
        }
        let e = self.expr()?;
        self.expect(P::Semicolon)?;
        Ok(Stmt::Expr(e))
    }

    // ---- expressions -----------------------------------------------------

    pub fn expr(&mut self) -> PResult<Expr> {
        let mut e = self.assign()?;
        while self.is(P::Comma) {
            let line = self.line();
            self.pos += 1;
            let r = self.assign()?;
            e = Expr::new(ExprKind::Comma(Box::new(e), Box::new(r)), line);
        }
        Ok(e)
    }

    fn assign(&mut self) -> PResult<Expr> {
        let lhs = self.ternary()?;
        let op = match self.peek() {
            Some(TokKind::Punct(p)) => match p {
                P::Assign => Some(None),
                P::AddAssign => Some(Some(BinOp::Add)),
                P::SubAssign => Some(Some(BinOp::Sub)),
                P::MulAssign => Some(Some(BinOp::Mul)),
                P::DivAssign => Some(Some(BinOp::Div)),
                P::ModAssign => Some(Some(BinOp::Mod)),
                P::ShlAssign => Some(Some(BinOp::Shl)),
                P::ShrAssign => Some(Some(BinOp::Shr)),
                P::AndAssign => Some(Some(BinOp::BitAnd)),
                P::OrAssign => Some(Some(BinOp::BitOr)),
                P::XorAssign => Some(Some(BinOp::BitXor)),
                _ => None,
            },
            _ => None,
        };
        if let Some(op) = op {
            let line = self.line();
            self.pos += 1;
            let rhs = self.assign()?;
            return Ok(Expr::new(
                ExprKind::Assign(op, Box::new(lhs), Box::new(rhs)),
                line,
            ));
        }
        Ok(lhs)
    }

    fn ternary(&mut self) -> PResult<Expr> {
        let c = self.binary(0)?;
        if self.is(P::Question) {
            let line = self.line();
            self.pos += 1;
            let a = self.expr()?;
            self.expect(P::Colon)?;
            let b = self.assign()?;
            return Ok(Expr::new(
                ExprKind::Ternary(Box::new(c), Box::new(a), Box::new(b)),
                line,
            ));
        }
        Ok(c)
    }

    fn binop_prec(p: P) -> Option<(u8, BinOp)> {
        Some(match p {
            P::OrOr => (1, BinOp::Or),
            P::XorXor => (2, BinOp::Xor),
            P::AndAnd => (3, BinOp::And),
            P::Pipe => (4, BinOp::BitOr),
            P::Caret => (5, BinOp::BitXor),
            P::Amp => (6, BinOp::BitAnd),
            P::EqEq => (7, BinOp::Eq),
            P::Ne => (7, BinOp::Ne),
            P::Lt => (8, BinOp::Lt),
            P::Gt => (8, BinOp::Gt),
            P::Le => (8, BinOp::Le),
            P::Ge => (8, BinOp::Ge),
            P::Shl => (9, BinOp::Shl),
            P::Shr => (9, BinOp::Shr),
            P::Plus => (10, BinOp::Add),
            P::Minus => (10, BinOp::Sub),
            P::Star => (11, BinOp::Mul),
            P::Slash => (11, BinOp::Div),
            P::Percent => (11, BinOp::Mod),
            _ => return None,
        })
    }

    fn binary(&mut self, min: u8) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        while let Some(TokKind::Punct(p)) = self.peek() {
            let Some((prec, op)) = Self::binop_prec(p) else {
                break;
            };
            if prec < min {
                break;
            }
            let line = self.line();
            self.pos += 1;
            let rhs = self.binary(prec + 1)?;
            lhs = Expr::new(ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), line);
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let line = self.line();
        let op = match self.peek() {
            Some(TokKind::Punct(P::Minus)) => Some(UnOp::Neg),
            Some(TokKind::Punct(P::Plus)) => Some(UnOp::Plus),
            Some(TokKind::Punct(P::Bang)) => Some(UnOp::Not),
            Some(TokKind::Punct(P::Tilde)) => Some(UnOp::BitNot),
            Some(TokKind::Punct(P::Inc)) => Some(UnOp::PreInc),
            Some(TokKind::Punct(P::Dec)) => Some(UnOp::PreDec),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            let e = self.unary()?;
            return Ok(Expr::new(ExprKind::Unary(op, Box::new(e)), line));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            let line = self.line();
            if self.eat(P::LBracket) {
                let i = self.expr()?;
                self.expect(P::RBracket)?;
                e = Expr::new(ExprKind::Index(Box::new(e), Box::new(i)), line);
            } else if self.eat(P::Dot) {
                let n = match self.peek() {
                    Some(TokKind::Ident(n)) => {
                        self.pos += 1;
                        n
                    }
                    _ => return self.err("expected field name after '.'"),
                };
                if self.int.get(n) == "length" && self.is(P::LParen) {
                    self.pos += 1;
                    self.expect(P::RParen)?;
                    e = Expr::new(ExprKind::Length(Box::new(e)), line);
                } else {
                    e = Expr::new(ExprKind::Field(Box::new(e), n), line);
                }
            } else if self.eat(P::Inc) {
                e = Expr::new(ExprKind::Unary(UnOp::PostInc, Box::new(e)), line);
            } else if self.eat(P::Dec) {
                e = Expr::new(ExprKind::Unary(UnOp::PostDec, Box::new(e)), line);
            } else {
                return Ok(e);
            }
        }
    }

    fn call_args(&mut self) -> PResult<Vec<Expr>> {
        self.expect(P::LParen)?;
        let mut args = Vec::new();
        if self.kw() == Some(Kw::Void) && self.peek_at(1) == Some(TokKind::Punct(P::RParen)) {
            self.pos += 1;
        }
        if self.eat(P::RParen) {
            return Ok(args);
        }
        loop {
            args.push(self.assign()?);
            if self.eat(P::Comma) {
                continue;
            }
            self.expect(P::RParen)?;
            return Ok(args);
        }
    }

    fn primary(&mut self) -> PResult<Expr> {
        let line = self.line();
        match self.peek() {
            Some(TokKind::Number(n)) => {
                self.pos += 1;
                let kind = parse_number(self.int.get(n)).ok_or_else(|| ParseError {
                    line,
                    msg: format!("invalid number '{}'", self.int.get(n)),
                })?;
                Ok(Expr::new(kind, line))
            }
            Some(TokKind::Punct(P::LParen)) => {
                self.pos += 1;
                let e = self.expr()?;
                self.expect(P::RParen)?;
                Ok(e)
            }
            Some(TokKind::Ident(id)) => {
                if self.kw() == Some(Kw::True) {
                    self.pos += 1;
                    return Ok(Expr::new(ExprKind::BoolLit(true), line));
                }
                if self.kw() == Some(Kw::False) {
                    self.pos += 1;
                    return Ok(Expr::new(ExprKind::BoolLit(false), line));
                }
                if self.is_type_at(0) {
                    let ty = self.full_type()?;
                    let args = self.call_args()?;
                    return Ok(Expr::new(
                        ExprKind::Call {
                            callee: Callee::Type(ty),
                            args,
                            target: CallTarget::Unresolved,
                        },
                        line,
                    ));
                }
                if Kw::from_id(id).is_some() {
                    return self.err(format!("unexpected keyword '{}'", self.int.get(id)));
                }
                self.pos += 1;
                if self.is(P::LParen) {
                    let args = self.call_args()?;
                    return Ok(Expr::new(
                        ExprKind::Call {
                            callee: Callee::Name(id),
                            args,
                            target: CallTarget::Unresolved,
                        },
                        line,
                    ));
                }
                Ok(Expr::new(ExprKind::Ident(id, Sym::Unresolved), line))
            }
            _ => {
                let f = self.describe_current();
                self.err(format!("expected expression, found {f}"))
            }
        }
    }
}

/// Parses a GLSL numeric literal.
pub fn parse_number(s: &str) -> Option<ExprKind> {
    let lower = s.to_ascii_lowercase();
    let is_hex = lower.starts_with("0x");
    if !is_hex && (lower.contains('.') || lower.contains('e') || lower.ends_with('f')) {
        let t = lower
            .strip_suffix("lf")
            .or_else(|| lower.strip_suffix('f'))
            .unwrap_or(&lower);
        let v: f64 = t.parse().ok()?;
        return Some(ExprKind::FloatLit(v as f32));
    }
    let (body, unsigned) = match lower.strip_suffix('u') {
        Some(b) => (b, true),
        None => (lower.as_str(), false),
    };
    let v = if let Some(h) = body.strip_prefix("0x") {
        u64::from_str_radix(h, 16).ok()?
    } else if body.len() > 1 && body.starts_with('0') {
        u64::from_str_radix(&body[1..], 8).ok()?
    } else {
        body.parse::<u64>().ok()?
    };
    if v > u32::MAX as u64 {
        return None;
    }
    Some(if unsigned {
        ExprKind::UintLit(v)
    } else {
        // Literals above i32::MAX wrap to the 32-bit pattern (glslang behaviour).
        ExprKind::IntLit(v as u32 as i32 as i64)
    })
}
