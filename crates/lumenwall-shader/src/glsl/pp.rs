//! C-style preprocessor for GLSL (object- and function-like macros with
//! Prosser hide sets, `##`, conditionals, `defined`).
//!
//! `#include` and `#require` are handled textually before this stage (spec 03
//! §4); any that survive are errors, like in the reference compiler.

use std::collections::HashMap;
use std::rc::Rc;

use super::lexer::{P, TokKind, Token, lex, spelling};
use super::types::{Interner, Name};

#[derive(Debug, Clone)]
pub struct PpError {
    pub line: u32,
    pub msg: String,
}

fn err<T>(line: u32, msg: impl Into<String>) -> Result<T, PpError> {
    Err(PpError {
        line,
        msg: msg.into(),
    })
}

struct Macro {
    params: Option<Vec<Name>>,
    body: Vec<Token>,
}

struct Cond {
    parent_active: bool,
    active: bool,
    taken: bool,
    seen_else: bool,
    line: u32,
}

pub struct Preprocessor<'i> {
    int: &'i mut Interner,
    macros: HashMap<Name, Rc<Macro>>,
    hidesets: Vec<Rc<[Name]>>,
    hs_map: HashMap<Rc<[Name]>, u32>,
    name_line: Name,
    name_defined: Name,
}

impl<'i> Preprocessor<'i> {
    pub fn new(int: &'i mut Interner) -> Self {
        let name_line = int.intern("__LINE__");
        let name_defined = int.intern("defined");
        let empty: Rc<[Name]> = Rc::from(Vec::new());
        let mut hs_map = HashMap::new();
        hs_map.insert(empty.clone(), 0);
        let mut pp = Preprocessor {
            int,
            macros: HashMap::new(),
            hidesets: vec![empty],
            hs_map,
            name_line,
            name_defined,
        };
        pp.define("__FILE__", "0");
        pp.define("__VERSION__", "330");
        pp.define("GL_core_profile", "1");
        pp
    }

    /// Defines an object-like macro.
    pub fn define(&mut self, name: &str, body: &str) {
        let n = self.int.intern(name);
        let mut toks = Vec::new();
        lex(body, self.int, 0, &mut toks);
        toks.retain(|t| t.kind != TokKind::Newline);
        if let Some(t) = toks.first_mut() {
            t.space = false;
        }
        self.macros.insert(
            n,
            Rc::new(Macro {
                params: None,
                body: toks,
            }),
        );
    }

    pub fn is_defined(&self, name: &str) -> bool {
        self.int
            .lookup(name)
            .is_some_and(|n| self.macros.contains_key(&n))
    }

    /// Runs the preprocessor over `src`; returns the token stream without newlines.
    pub fn run(&mut self, src: &str) -> Result<Vec<Token>, PpError> {
        let mut toks = Vec::with_capacity(src.len() / 3);
        lex(src, self.int, 1, &mut toks);
        let mut out = Vec::with_capacity(toks.len());
        let mut conds: Vec<Cond> = Vec::new();
        let mut run: Vec<Token> = Vec::new();
        let mut at_line_start = true;
        let mut i = 0;
        while i < toks.len() {
            let t = toks[i];
            if t.kind == TokKind::Newline {
                at_line_start = true;
                i += 1;
                continue;
            }
            let active = conds.last().is_none_or(|c| c.active);
            if at_line_start && t.is_punct(P::Hash) {
                let end = toks[i..]
                    .iter()
                    .position(|t| t.kind == TokKind::Newline)
                    .map_or(toks.len(), |p| i + p);
                if !run.is_empty() {
                    let r = std::mem::take(&mut run);
                    out.extend(self.expand(r)?);
                }
                let line: Vec<Token> = toks[i + 1..end].to_vec();
                self.directive(&line, t.line, &mut conds, active)?;
                i = end;
                continue;
            }
            at_line_start = false;
            if active {
                run.push(t);
            }
            i += 1;
        }
        if let Some(c) = conds.last() {
            return err(c.line, "unterminated #if");
        }
        if !run.is_empty() {
            out.extend(self.expand(run)?);
        }
        Ok(out)
    }

    fn ident_text(&self, t: &Token) -> Option<&str> {
        match t.kind {
            TokKind::Ident(n) => Some(self.int.get(n)),
            _ => None,
        }
    }

    fn directive(
        &mut self,
        line: &[Token],
        lno: u32,
        conds: &mut Vec<Cond>,
        active: bool,
    ) -> Result<(), PpError> {
        let Some(first) = line.first() else {
            return Ok(()); // null directive
        };
        let Some(name) = self.ident_text(first).map(str::to_owned) else {
            if active {
                return err(lno, "invalid preprocessor directive");
            }
            return Ok(());
        };
        let rest = &line[1..];
        match name.as_str() {
            "if" | "ifdef" | "ifndef" => {
                let value = if active {
                    match name.as_str() {
                        "if" => self.eval_if(rest, lno)? != 0,
                        _ => {
                            let Some(n) = rest.first().and_then(|t| match t.kind {
                                TokKind::Ident(n) => Some(n),
                                _ => None,
                            }) else {
                                return err(lno, format!("#{name} expects an identifier"));
                            };
                            let d = self.macros.contains_key(&n) || n == self.name_line;
                            if name == "ifdef" { d } else { !d }
                        }
                    }
                } else {
                    false
                };
                conds.push(Cond {
                    parent_active: active,
                    active: active && value,
                    taken: value,
                    seen_else: false,
                    line: lno,
                });
            }
            "elif" => {
                let Some(c) = conds.last() else {
                    return err(lno, "#elif without #if");
                };
                if c.seen_else {
                    return err(lno, "#elif after #else");
                }
                let (parent, taken) = (c.parent_active, c.taken);
                let value = if parent && !taken {
                    self.eval_if(rest, lno)? != 0
                } else {
                    false
                };
                let c = conds.last_mut().expect("checked");
                c.active = parent && !taken && value;
                c.taken |= value;
            }
            "else" => {
                let Some(c) = conds.last_mut() else {
                    return err(lno, "#else without #if");
                };
                if c.seen_else {
                    return err(lno, "duplicate #else");
                }
                c.seen_else = true;
                c.active = c.parent_active && !c.taken;
                c.taken = true;
            }
            "endif" => {
                if conds.pop().is_none() {
                    return err(lno, "#endif without #if");
                }
            }
            _ if !active => {}
            "define" => self.parse_define(rest, lno)?,
            "undef" => {
                if let Some(TokKind::Ident(n)) = rest.first().map(|t| t.kind) {
                    self.macros.remove(&n);
                } else {
                    return err(lno, "#undef expects an identifier");
                }
            }
            "error" => {
                let msg: Vec<String> = rest.iter().map(|t| spelling(t, self.int)).collect();
                return err(lno, format!("#error {}", msg.join(" ")));
            }
            "pragma" | "extension" | "line" => {}
            "version" => return err(lno, "#version is not allowed in wallpaper shader sources"),
            other => return err(lno, format!("unknown preprocessor directive #{other}")),
        }
        Ok(())
    }

    fn parse_define(&mut self, rest: &[Token], lno: u32) -> Result<(), PpError> {
        let Some(TokKind::Ident(name)) = rest.first().map(|t| t.kind) else {
            return err(lno, "#define expects an identifier");
        };
        let mut i = 1;
        let mut params = None;
        if rest
            .get(1)
            .is_some_and(|t| t.is_punct(P::LParen) && !t.space)
        {
            let mut ps = Vec::new();
            i = 2;
            loop {
                match rest.get(i).map(|t| t.kind) {
                    Some(TokKind::Punct(P::RParen)) if ps.is_empty() => {
                        i += 1;
                        break;
                    }
                    Some(TokKind::Ident(p)) => {
                        ps.push(p);
                        i += 1;
                        match rest.get(i).map(|t| t.kind) {
                            Some(TokKind::Punct(P::Comma)) => i += 1,
                            Some(TokKind::Punct(P::RParen)) => {
                                i += 1;
                                break;
                            }
                            _ => return err(lno, "malformed macro parameter list"),
                        }
                    }
                    _ => return err(lno, "malformed macro parameter list"),
                }
            }
            params = Some(ps);
        }
        let mut body: Vec<Token> = rest[i..].to_vec();
        if let Some(t) = body.first_mut() {
            t.space = false;
        }
        self.macros.insert(name, Rc::new(Macro { params, body }));
        Ok(())
    }

    // ---- hide sets -------------------------------------------------------

    fn hs_intern(&mut self, v: Vec<Name>) -> u32 {
        if let Some(&id) = self.hs_map.get(v.as_slice()) {
            return id;
        }
        let rc: Rc<[Name]> = Rc::from(v);
        let id = self.hidesets.len() as u32;
        self.hidesets.push(rc.clone());
        self.hs_map.insert(rc, id);
        id
    }
    fn hs_contains(&self, hs: u32, n: Name) -> bool {
        self.hidesets[hs as usize].binary_search(&n).is_ok()
    }
    fn hs_union(&mut self, a: u32, b: u32) -> u32 {
        if a == b || b == 0 {
            return a;
        }
        if a == 0 {
            return b;
        }
        let mut v: Vec<Name> = self.hidesets[a as usize].to_vec();
        v.extend_from_slice(&self.hidesets[b as usize]);
        v.sort_unstable();
        v.dedup();
        self.hs_intern(v)
    }
    fn hs_add(&mut self, a: u32, n: Name) -> u32 {
        let single = self.hs_intern(vec![n]);
        self.hs_union(a, single)
    }
    fn hs_intersect(&mut self, a: u32, b: u32) -> u32 {
        if a == b {
            return a;
        }
        let bb = self.hidesets[b as usize].clone();
        let v: Vec<Name> = self.hidesets[a as usize]
            .iter()
            .copied()
            .filter(|n| bb.binary_search(n).is_ok())
            .collect();
        self.hs_intern(v)
    }

    // ---- expansion -------------------------------------------------------

    /// Fully macro-expands a token list (Prosser's algorithm).
    fn expand(&mut self, input: Vec<Token>) -> Result<Vec<Token>, PpError> {
        let mut stack: Vec<Token> = input;
        stack.reverse();
        let mut out = Vec::with_capacity(stack.len());
        let mut guard = 0u32;
        while let Some(t) = stack.pop() {
            let TokKind::Ident(name) = t.kind else {
                out.push(t);
                continue;
            };
            if name == self.name_line {
                let n = self.int.intern(&t.line.to_string());
                out.push(Token {
                    kind: TokKind::Number(n),
                    ..t
                });
                continue;
            }
            let Some(m) = self.macros.get(&name).cloned() else {
                out.push(t);
                continue;
            };
            if self.hs_contains(t.hs, name) {
                out.push(t);
                continue;
            }
            guard += 1;
            if guard > 1_000_000 {
                return err(t.line, "macro expansion limit exceeded");
            }
            match &m.params {
                None => {
                    let hs = self.hs_add(t.hs, name);
                    let body = self.subst(&m.body, &[], &[], hs, &t)?;
                    stack.extend(body.into_iter().rev());
                }
                Some(params) => {
                    if !stack.last().is_some_and(|n| n.is_punct(P::LParen)) {
                        out.push(t);
                        continue;
                    }
                    stack.pop();
                    let mut args: Vec<Vec<Token>> = vec![Vec::new()];
                    let mut depth = 0u32;
                    let rparen = loop {
                        let Some(a) = stack.pop() else {
                            return err(t.line, "unterminated macro invocation");
                        };
                        match a.kind {
                            TokKind::Punct(P::LParen) => depth += 1,
                            TokKind::Punct(P::RParen) if depth == 0 => break a,
                            TokKind::Punct(P::RParen) => depth -= 1,
                            TokKind::Punct(P::Comma) if depth == 0 => {
                                args.push(Vec::new());
                                continue;
                            }
                            _ => {}
                        }
                        args.last_mut().expect("non-empty").push(a);
                    };
                    if params.is_empty() && args.len() == 1 && args[0].is_empty() {
                        args.clear();
                    }
                    if args.len() != params.len() {
                        return err(
                            t.line,
                            format!(
                                "macro '{}' expects {} arguments, got {}",
                                self.int.get(name),
                                params.len(),
                                args.len()
                            ),
                        );
                    }
                    let inter = self.hs_intersect(t.hs, rparen.hs);
                    let hs = self.hs_add(inter, name);
                    let body = self.subst(&m.body, params, &args, hs, &t)?;
                    stack.extend(body.into_iter().rev());
                }
            }
        }
        Ok(out)
    }

    fn subst(
        &mut self,
        body: &[Token],
        params: &[Name],
        args: &[Vec<Token>],
        hs: u32,
        call: &Token,
    ) -> Result<Vec<Token>, PpError> {
        let param_index = |t: &Token| match t.kind {
            TokKind::Ident(n) => params.iter().position(|&p| p == n),
            _ => None,
        };
        let mut res: Vec<Token> = Vec::with_capacity(body.len());
        let mut i = 0;
        while i < body.len() {
            let t = body[i];
            if t.is_punct(P::HashHash) && i + 1 < body.len() {
                let rhs_t = body[i + 1];
                let rhs: Vec<Token> = match param_index(&rhs_t) {
                    Some(p) => args[p].clone(),
                    None => vec![rhs_t],
                };
                match (res.pop(), rhs.first()) {
                    (Some(lhs), Some(first)) => {
                        let pasted = self.paste(&lhs, first, call.line)?;
                        res.push(pasted);
                        res.extend_from_slice(&rhs[1..]);
                    }
                    (Some(lhs), None) => res.push(lhs),
                    (None, _) => res.extend(rhs),
                }
                i += 2;
                continue;
            }
            if let Some(p) = param_index(&t) {
                let next_paste = body.get(i + 1).is_some_and(|n| n.is_punct(P::HashHash));
                let mut a = if next_paste {
                    args[p].clone()
                } else {
                    self.expand(args[p].clone())?
                };
                if let Some(f) = a.first_mut() {
                    f.space = t.space;
                }
                res.extend(a);
                i += 1;
                continue;
            }
            res.push(t);
            i += 1;
        }
        for (k, r) in res.iter_mut().enumerate() {
            r.hs = self.hs_union(r.hs, hs);
            r.line = call.line;
            if k == 0 {
                r.space = call.space;
            }
        }
        Ok(res)
    }

    fn paste(&mut self, a: &Token, b: &Token, line: u32) -> Result<Token, PpError> {
        let s = format!("{}{}", spelling(a, self.int), spelling(b, self.int));
        let mut toks = Vec::new();
        lex(&s, self.int, line, &mut toks);
        if toks.len() != 1 {
            return err(line, format!("'##' produced an invalid token '{s}'"));
        }
        Ok(Token {
            space: a.space,
            hs: 0,
            ..toks[0]
        })
    }

    // ---- #if evaluation --------------------------------------------------

    fn eval_if(&mut self, toks: &[Token], lno: u32) -> Result<i64, PpError> {
        // Replace `defined X` / `defined(X)` before expansion.
        let mut pre = Vec::with_capacity(toks.len());
        let mut i = 0;
        let one = self.int.intern("1");
        let zero = self.int.intern("0");
        while i < toks.len() {
            let t = toks[i];
            if t.kind == TokKind::Ident(self.name_defined) {
                let (n, adv) = match (toks.get(i + 1), toks.get(i + 2), toks.get(i + 3)) {
                    (Some(a), Some(b), Some(c))
                        if a.is_punct(P::LParen) && c.is_punct(P::RParen) =>
                    {
                        (b, 4)
                    }
                    (Some(a), _, _) => (a, 2),
                    _ => return err(lno, "'defined' without identifier"),
                };
                let TokKind::Ident(n) = n.kind else {
                    return err(lno, "'defined' expects an identifier");
                };
                let d = self.macros.contains_key(&n) || n == self.name_line;
                pre.push(Token {
                    kind: TokKind::Number(if d { one } else { zero }),
                    ..t
                });
                i += adv;
                continue;
            }
            pre.push(t);
            i += 1;
        }
        let expanded = self.expand(pre)?;
        let mut ev = IfEval {
            toks: &expanded,
            pos: 0,
            int: self.int,
            line: lno,
        };
        let v = ev.ternary()?;
        if ev.pos != expanded.len() {
            return err(lno, "unexpected tokens in #if expression");
        }
        Ok(v)
    }
}

struct IfEval<'a> {
    toks: &'a [Token],
    pos: usize,
    int: &'a Interner,
    line: u32,
}

impl IfEval<'_> {
    fn peek(&self) -> Option<TokKind> {
        self.toks.get(self.pos).map(|t| t.kind)
    }
    fn eat(&mut self, p: P) -> bool {
        if self.peek() == Some(TokKind::Punct(p)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn ternary(&mut self) -> Result<i64, PpError> {
        let c = self.binary(0)?;
        if self.eat(P::Question) {
            let a = self.ternary()?;
            if !self.eat(P::Colon) {
                return err(self.line, "expected ':' in #if expression");
            }
            let b = self.ternary()?;
            return Ok(if c != 0 { a } else { b });
        }
        Ok(c)
    }
    fn binary(&mut self, min_prec: u8) -> Result<i64, PpError> {
        let mut lhs = self.unary()?;
        while let Some(TokKind::Punct(p)) = self.peek() {
            let prec = match p {
                P::OrOr => 1,
                P::AndAnd => 2,
                P::Pipe => 3,
                P::Caret => 4,
                P::Amp => 5,
                P::EqEq | P::Ne => 6,
                P::Lt | P::Gt | P::Le | P::Ge => 7,
                P::Shl | P::Shr => 8,
                P::Plus | P::Minus => 9,
                P::Star | P::Slash | P::Percent => 10,
                _ => break,
            };
            if prec < min_prec || prec == 0 {
                break;
            }
            self.pos += 1;
            let rhs = self.binary(prec + 1)?;
            lhs = match p {
                P::OrOr => ((lhs != 0) || (rhs != 0)) as i64,
                P::AndAnd => ((lhs != 0) && (rhs != 0)) as i64,
                P::Pipe => lhs | rhs,
                P::Caret => lhs ^ rhs,
                P::Amp => lhs & rhs,
                P::EqEq => (lhs == rhs) as i64,
                P::Ne => (lhs != rhs) as i64,
                P::Lt => (lhs < rhs) as i64,
                P::Gt => (lhs > rhs) as i64,
                P::Le => (lhs <= rhs) as i64,
                P::Ge => (lhs >= rhs) as i64,
                P::Shl => lhs.wrapping_shl(rhs as u32),
                P::Shr => lhs.wrapping_shr(rhs as u32),
                P::Plus => lhs.wrapping_add(rhs),
                P::Minus => lhs.wrapping_sub(rhs),
                P::Star => lhs.wrapping_mul(rhs),
                P::Slash | P::Percent => {
                    if rhs == 0 {
                        return err(self.line, "division by zero in #if expression");
                    }
                    if p == P::Slash {
                        lhs.wrapping_div(rhs)
                    } else {
                        lhs.wrapping_rem(rhs)
                    }
                }
                _ => unreachable!(),
            };
        }
        Ok(lhs)
    }
    fn unary(&mut self) -> Result<i64, PpError> {
        match self.peek() {
            Some(TokKind::Punct(P::Minus)) => {
                self.pos += 1;
                Ok(self.unary()?.wrapping_neg())
            }
            Some(TokKind::Punct(P::Plus)) => {
                self.pos += 1;
                self.unary()
            }
            Some(TokKind::Punct(P::Bang)) => {
                self.pos += 1;
                Ok((self.unary()? == 0) as i64)
            }
            Some(TokKind::Punct(P::Tilde)) => {
                self.pos += 1;
                Ok(!self.unary()?)
            }
            Some(TokKind::Punct(P::LParen)) => {
                self.pos += 1;
                let v = self.ternary()?;
                if !self.eat(P::RParen) {
                    return err(self.line, "expected ')' in #if expression");
                }
                Ok(v)
            }
            Some(TokKind::Number(n)) => {
                self.pos += 1;
                parse_pp_int(self.int.get(n)).ok_or_else(|| PpError {
                    line: self.line,
                    msg: "invalid number in #if".into(),
                })
            }
            Some(TokKind::Ident(_)) => {
                // Undefined identifiers evaluate to 0 (spec 03 §5.3: `#if MASK` is 0).
                self.pos += 1;
                Ok(0)
            }
            _ => err(self.line, "malformed #if expression"),
        }
    }
}

/// Parses an integer preprocessing number (decimal, hex, octal, optional `u`).
pub fn parse_pp_int(s: &str) -> Option<i64> {
    let s = s.strip_suffix(['u', 'U']).unwrap_or(s);
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u64::from_str_radix(h, 16).ok().map(|v| v as i64);
    }
    if s.len() > 1 && s.starts_with('0') {
        return u64::from_str_radix(&s[1..], 8).ok().map(|v| v as i64);
    }
    s.parse::<u64>().ok().map(|v| v as i64)
}
