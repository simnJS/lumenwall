//! Merges the fragment module into the vertex module, so one naga module
//! carries both entry points (the renderer creates one wgpu module per pass).
//! Bound resources with the same `(group, binding)` become one global.

use naga::{
    Block, Expression, Function, GlobalVariable, Handle, Module, Statement, StructMember, Type,
    TypeInner,
};

struct Maps {
    types: Vec<Handle<Type>>,
    constants: Vec<Handle<naga::Constant>>,
    gexprs: Vec<Handle<Expression>>,
    globals: Vec<Handle<GlobalVariable>>,
    functions: Vec<Handle<Function>>,
}

fn remap_type(inner: &mut TypeInner, m: &[Handle<Type>]) -> Result<(), String> {
    match inner {
        TypeInner::Pointer { base, .. } | TypeInner::BindingArray { base, .. } => {
            *base = m[base.index()]
        }
        TypeInner::Array { base, size, .. } => {
            if matches!(size, naga::ArraySize::Pending(_)) {
                return Err("override-sized array".into());
            }
            *base = m[base.index()];
        }
        TypeInner::Struct { members, .. } => {
            for StructMember { ty, .. } in members.iter_mut() {
                *ty = m[ty.index()];
            }
        }
        _ => {}
    }
    Ok(())
}

/// Cross-arena handles of an expression (types, constants, globals, functions).
fn remap_expr_refs(e: &mut Expression, m: &Maps) -> Result<(), String> {
    match e {
        Expression::Constant(h) => *h = m.constants[h.index()],
        Expression::Override(_) => return Err("overrides are not supported".into()),
        Expression::ZeroValue(t) => *t = m.types[t.index()],
        Expression::Compose { ty, .. } => *ty = m.types[ty.index()],
        Expression::GlobalVariable(h) => *h = m.globals[h.index()],
        Expression::CallResult(f) => *f = m.functions[f.index()],
        Expression::AtomicResult { ty, .. }
        | Expression::WorkGroupUniformLoadResult { ty }
        | Expression::SubgroupOperationResult { ty } => *ty = m.types[ty.index()],
        _ => {}
    }
    Ok(())
}

/// Global (constant) expressions also reference each other.
fn remap_global_expr(e: &mut Expression, m: &Maps) -> Result<(), String> {
    remap_expr_refs(e, m)?;
    let g = |h: &mut Handle<Expression>| *h = m.gexprs[h.index()];
    match e {
        Expression::Literal(_) | Expression::Constant(_) | Expression::ZeroValue(_) => {}
        Expression::Compose { components, .. } => components.iter_mut().for_each(g),
        Expression::Access { base, index } => {
            g(base);
            g(index);
        }
        Expression::AccessIndex { base, .. } => g(base),
        Expression::Splat { value, .. } => g(value),
        Expression::Swizzle { vector, .. } => g(vector),
        Expression::Unary { expr, .. } | Expression::As { expr, .. } => g(expr),
        Expression::Binary { left, right, .. } => {
            g(left);
            g(right);
        }
        Expression::Select {
            condition,
            accept,
            reject,
        } => {
            g(condition);
            g(accept);
            g(reject);
        }
        Expression::Relational { argument, .. } => g(argument),
        Expression::Math {
            arg,
            arg1,
            arg2,
            arg3,
            ..
        } => {
            g(arg);
            for a in [arg1, arg2, arg3].into_iter().flatten() {
                g(a);
            }
        }
        other => return Err(format!("unexpected constant expression {other:?}")),
    }
    Ok(())
}

fn remap_block(b: &mut Block, m: &Maps) {
    for s in b.iter_mut() {
        match s {
            Statement::Block(inner) => remap_block(inner, m),
            Statement::If { accept, reject, .. } => {
                remap_block(accept, m);
                remap_block(reject, m);
            }
            Statement::Switch { cases, .. } => {
                cases.iter_mut().for_each(|c| remap_block(&mut c.body, m))
            }
            Statement::Loop {
                body, continuing, ..
            } => {
                remap_block(body, m);
                remap_block(continuing, m);
            }
            Statement::Call { function, .. } => *function = m.functions[function.index()],
            _ => {}
        }
    }
}

fn remap_function(f: &mut Function, m: &Maps) -> Result<(), String> {
    if f.diagnostic_filter_leaf.is_some() {
        return Err("diagnostic filters are not supported".into());
    }
    for a in f.arguments.iter_mut() {
        a.ty = m.types[a.ty.index()];
    }
    if let Some(r) = f.result.as_mut() {
        r.ty = m.types[r.ty.index()];
    }
    for (_, l) in f.local_variables.iter_mut() {
        l.ty = m.types[l.ty.index()];
    }
    for (_, e) in f.expressions.iter_mut() {
        remap_expr_refs(e, m)?;
    }
    remap_block(&mut f.body, m);
    Ok(())
}

/// Appends `b` (one entry point) into `a`.
pub fn merge(mut a: Module, b: Module) -> Result<Module, String> {
    if !b.overrides.is_empty() || !b.diagnostic_filters.is_empty() {
        return Err("overrides and diagnostic filters are not supported".into());
    }
    let mut types = Vec::with_capacity(b.types.len());
    for (h, t) in b.types.iter() {
        let mut t = t.clone();
        remap_type(&mut t.inner, &types)?;
        types.push(a.types.insert(t, b.types.get_span(h)));
    }
    for (k, h) in b.special_types.predeclared_types.iter() {
        if !a.special_types.predeclared_types.contains_key(k) {
            a.special_types
                .predeclared_types
                .insert(k.clone(), types[h.index()]);
        }
    }
    // Reserve handles first (items reference each other), then fix them up.
    let constants: Vec<_> = b
        .constants
        .iter()
        .map(|(h, c)| a.constants.append(c.clone(), b.constants.get_span(h)))
        .collect();
    let gexprs: Vec<_> = b
        .global_expressions
        .iter()
        .map(|(h, e)| {
            a.global_expressions
                .append(e.clone(), b.global_expressions.get_span(h))
        })
        .collect();
    let mut globals = Vec::with_capacity(b.global_variables.len());
    let mut appended_globals = Vec::new();
    for (h, g) in b.global_variables.iter() {
        let shared = g.binding.as_ref().and_then(|rb| {
            a.global_variables
                .iter()
                .find(|(_, x)| x.binding.as_ref() == Some(rb))
                .map(|(ah, x)| (ah, x.ty))
        });
        match shared {
            Some((ah, aty)) => {
                if aty != types[g.ty.index()] {
                    return Err(format!(
                        "binding {:?} has different types in the two stages",
                        g.binding
                    ));
                }
                globals.push(ah);
            }
            None => {
                let nh = a
                    .global_variables
                    .append(g.clone(), b.global_variables.get_span(h));
                appended_globals.push(nh);
                globals.push(nh);
            }
        }
    }
    let functions: Vec<_> = b
        .functions
        .iter()
        .map(|(h, f)| a.functions.append(f.clone(), b.functions.get_span(h)))
        .collect();
    let m = Maps {
        types,
        constants,
        gexprs,
        globals,
        functions,
    };
    for &h in &m.constants {
        let c = &mut a.constants[h];
        c.ty = m.types[c.ty.index()];
        c.init = m.gexprs[c.init.index()];
    }
    for &h in &m.gexprs {
        remap_global_expr(&mut a.global_expressions[h], &m)?;
    }
    for h in appended_globals {
        let g = &mut a.global_variables[h];
        g.ty = m.types[g.ty.index()];
        g.init = g.init.map(|i| m.gexprs[i.index()]);
    }
    for &h in &m.functions {
        remap_function(&mut a.functions[h], &m)?;
    }
    for ep in b.entry_points {
        if ep.workgroup_size_overrides.is_some()
            || ep.task_payload.is_some()
            || ep.mesh_info.is_some()
        {
            return Err("unsupported entry point features".into());
        }
        let mut ep = ep;
        remap_function(&mut ep.function, &m)?;
        a.entry_points.push(ep);
    }
    Ok(a)
}
