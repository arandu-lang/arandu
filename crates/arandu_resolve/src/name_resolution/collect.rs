use arandu_parser::{FuncName, ImportDecl, TopLevelDecl, Visibility};

use crate::{DiagCode, Diagnostic, ScopeId, SymbolKind};

use super::Resolver;
use super::util::is_type_case;

impl<'a> Resolver<'a> {
    pub(crate) fn collect_import(&mut self, scope: ScopeId, import: &ImportDecl) {
        match import {
            ImportDecl::ModuleAlias { span, alias, .. } => {
                // Import aliases are file-local (never re-exported via this name).
                if let Some(sym) = self.define(scope, alias, SymbolKind::Module, *span) {
                    // SmolStr::clone is O(1)
                    self.record_import_symbol(sym, alias.clone(), *span);
                }
            }
            ImportDecl::Named { items, .. } => {
                for item in items {
                    let name = item.alias.as_ref().unwrap_or(&item.name);
                    let kind = if is_type_case(name) {
                        SymbolKind::ImportType
                    } else {
                        SymbolKind::ImportValue
                    };
                    if let Some(sym) = self.define(scope, name, kind, item.span) {
                        // SmolStr::clone is O(1)
                        self.record_import_symbol(sym, name.clone(), item.span);
                    }
                }
            }
            ImportDecl::ReExport {
                items, visibility, ..
            } => {
                for item in items {
                    let name = item.alias.as_ref().unwrap_or(&item.name);
                    let kind = if is_type_case(name) {
                        SymbolKind::ImportType
                    } else {
                        SymbolKind::ImportValue
                    };
                    if let Some(sym) = self.define(scope, name, kind, item.span)
                        && *visibility == Visibility::Private
                    {
                        self.record_import_symbol(sym, name.clone(), item.span);
                    }
                }
            }
            ImportDecl::ExternalAlias {
                span,
                source,
                alias,
            } => {
                if let Some(sym) = self.define(scope, alias, SymbolKind::Module, *span) {
                    // SmolStr::clone is O(1)
                    self.record_import_symbol(sym, alias.clone(), *span);
                }
                // SmolStr::clone is O(1)
                self.import_aliases.insert(alias.clone(), source.clone());
            }
            ImportDecl::ExternalNamed { items, .. } => {
                for item in items {
                    let name = item.alias.as_ref().unwrap_or(&item.name);
                    let kind = if is_type_case(name) {
                        SymbolKind::ImportType
                    } else {
                        SymbolKind::ImportValue
                    };
                    if let Some(sym) = self.define(scope, name, kind, item.span) {
                        self.record_import_symbol(sym, name.clone(), item.span);
                    }
                }
            }
        }
    }

    pub(crate) fn collect_top_level(&mut self, scope: ScopeId, decl: &TopLevelDecl) {
        match decl {
            TopLevelDecl::Const(decl) => {
                self.define_with_visibility(
                    scope,
                    &decl.name,
                    SymbolKind::Const,
                    decl.span,
                    decl.visibility,
                );
            }
            TopLevelDecl::TypeAlias(decl) => {
                self.define_with_visibility(
                    scope,
                    &decl.name,
                    SymbolKind::TypeAlias,
                    decl.span,
                    decl.visibility,
                );
            }
            TopLevelDecl::Func(decl) => match &decl.name {
                FuncName::Free { span, name } => {
                    let sym = self.define_with_visibility(
                        scope,
                        name,
                        SymbolKind::Func,
                        *span,
                        decl.visibility,
                    );
                    if let Some(sym) = sym {
                        use arandu_middle::symbol_table::LangItem;
                        let item = match (self.current_module.as_deref(), name.as_str()) {
                            (Some("std.thread"), "spawn") => Some(LangItem::ThreadSpawn),
                            (Some("std.thread"), "parkTimeout") => {
                                Some(LangItem::ThreadParkTimeout)
                            }
                            (Some("std.thread"), "park") => Some(LangItem::ThreadPark),
                            (Some("std.thread"), "unpark") => Some(LangItem::ThreadUnpark),
                            _ => None,
                        };
                        if let Some(item) = item {
                            self.symbols.set_lang_item(sym, item);
                        }
                    }
                }
                FuncName::Method {
                    span,
                    receiver,
                    name,
                } => {
                    let receiver_str = receiver.path.join(".");
                    let method_name = format!("{receiver_str}.{name}");
                    match self.symbols.define_with_visibility(
                        scope,
                        &method_name,
                        SymbolKind::AssociatedFunc,
                        *span,
                        decl.visibility,
                    ) {
                        Ok(symbol) => {
                            self.resolved.define(*span, symbol);
                            if let Some(type_sym) = self.symbols.lookup_type(scope, &receiver_str) {
                                self.symbols
                                    .associated_members
                                    .insert((type_sym, name.clone()), symbol);
                            }
                            if let Some(item) =
                                match (self.current_module.as_deref(), method_name.as_str()) {
                                    (Some("std.thread"), "Thread.unpark") => {
                                        Some(arandu_middle::symbol_table::LangItem::ThreadUnpark)
                                    }
                                    _ => None,
                                }
                            {
                                self.symbols.set_lang_item(symbol, item);
                            }
                        }
                        Err(previous) => {
                            let previous_symbol = self.symbols.get(previous);
                            self.diagnostics.push(
                                Diagnostic::error(
                                    DiagCode::N003RedefinedName,
                                    format!(
                                        "associated function '{receiver_str}.{name}' is already declared"
                                    ),
                                    *span,
                                )
                                .with_label(previous_symbol.span, "previous declaration is here"),
                            );
                        }
                    }
                }
            },
            TopLevelDecl::Struct(decl) => {
                let symbol = self.define_with_visibility(
                    scope,
                    &decl.name,
                    SymbolKind::Struct,
                    decl.span,
                    decl.visibility,
                );
                if let Some(symbol) = symbol {
                    use arandu_middle::symbol_table::LangItem;
                    let item = match (self.current_module.as_deref(), decl.name.as_str()) {
                        (Some("std.runtime.executor"), "TaskHandle") => Some(LangItem::TaskHandle),
                        (Some("std.alloc.string"), "String") => Some(LangItem::String),
                        (Some("std.alloc.vec"), "Vec") => Some(LangItem::Vec),
                        (Some("std.alloc.arc"), "Arc") => Some(LangItem::Arc),
                        (Some("std.alloc.arc"), "ArcWeak") => Some(LangItem::ArcWeak),
                        (Some("std.core.simd"), "F32x4") => Some(LangItem::VectorF32x4),
                        (Some("std.core.simd"), "F64x2") => Some(LangItem::VectorF64x2),
                        (Some("std.core.simd"), "I32x4") => Some(LangItem::VectorI32x4),
                        (Some("std.core.simd"), "I16x8") => Some(LangItem::VectorI16x8),
                        (Some("std.core.simd"), "U8x16") => Some(LangItem::VectorU8x16),
                        (Some("std.core.simd"), "I64x2") => Some(LangItem::VectorI64x2),
                        (Some("std.sync.condvar"), "Condvar") => Some(LangItem::Condvar),
                        (Some("std.core.cell"), "UnsafeCell") => Some(LangItem::UnsafeCell),
                        (Some("std.sync.rwlock"), "RwLock") => Some(LangItem::RwLock),
                        (Some("std.sync.rwlock"), "RwLockReadGuard") => {
                            Some(LangItem::RwLockReadGuard)
                        }
                        (Some("std.sync.rwlock"), "RwLockWriteGuard") => {
                            Some(LangItem::RwLockWriteGuard)
                        }
                        (Some("std.sync.mutex"), "Mutex") => Some(LangItem::Mutex),
                        (Some("std.sync.mutex"), "MutexGuard") => Some(LangItem::MutexGuard),
                        _ => None,
                    };
                    if let Some(item) = item {
                        self.symbols.set_lang_item(symbol, item);
                    }
                }
            }
            TopLevelDecl::Enum(decl) => {
                let visibility = decl.visibility;
                if let Some(enum_sym) = self.define_with_visibility(
                    scope,
                    &decl.name,
                    SymbolKind::Enum,
                    decl.span,
                    visibility,
                ) {
                    match (self.current_module.as_deref(), decl.name.as_str()) {
                        (Some("std.core.future"), "Poll") => {
                            self.symbols.set_lang_item(
                                enum_sym,
                                arandu_middle::symbol_table::LangItem::Poll,
                            );
                        }
                        (Some("std.core.result"), "Result") => {
                            self.symbols.set_lang_item(
                                enum_sym,
                                arandu_middle::symbol_table::LangItem::Result,
                            );
                        }
                        (Some("std.core.option"), "Option") => {
                            self.symbols.set_lang_item(
                                enum_sym,
                                arandu_middle::symbol_table::LangItem::Option,
                            );
                        }
                        (Some("std.core.coroutine"), "Coroutine") => {
                            self.symbols.set_lang_item(
                                enum_sym,
                                arandu_middle::symbol_table::LangItem::Coroutine,
                            );
                        }
                        _ => {}
                    }
                    // Variants inherit the enum's export visibility (public enum → public ctors).
                    for variant in &decl.variants {
                        if let Ok(symbol) = self.symbols.define_associated_member_with_visibility(
                            enum_sym,
                            &variant.name,
                            variant.span,
                            visibility,
                        ) {
                            self.resolved.define(variant.span, symbol);
                            match (
                                self.current_module.as_deref(),
                                decl.name.as_str(),
                                variant.name.as_str(),
                            ) {
                                (Some("std.core.option"), "Option", "Some") => {
                                    self.symbols.set_lang_item(
                                        symbol,
                                        arandu_middle::symbol_table::LangItem::OptionSome,
                                    );
                                }
                                (Some("std.core.option"), "Option", "None") => {
                                    self.symbols.set_lang_item(
                                        symbol,
                                        arandu_middle::symbol_table::LangItem::OptionNone,
                                    );
                                }
                                (Some("std.core.result"), "Result", "Ok") => {
                                    self.symbols.set_lang_item(
                                        symbol,
                                        arandu_middle::symbol_table::LangItem::ResultOk,
                                    );
                                }
                                (Some("std.core.result"), "Result", "Err") => {
                                    self.symbols.set_lang_item(
                                        symbol,
                                        arandu_middle::symbol_table::LangItem::ResultErr,
                                    );
                                }
                                (Some("std.core.future"), "Poll", "Ready") => {
                                    self.symbols.set_lang_item(
                                        symbol,
                                        arandu_middle::symbol_table::LangItem::PollReady,
                                    );
                                }
                                (Some("std.core.future"), "Poll", "Pending") => {
                                    self.symbols.set_lang_item(
                                        symbol,
                                        arandu_middle::symbol_table::LangItem::PollPending,
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            TopLevelDecl::Interface(decl) => {
                let symbol = self.define_with_visibility(
                    scope,
                    &decl.name,
                    SymbolKind::Interface,
                    decl.span,
                    decl.visibility,
                );
                if let Some(symbol) = symbol {
                    if decl.sealed {
                        self.symbols.sealed_interfaces.insert(symbol);
                    }
                    use arandu_middle::symbol_table::LangItem;
                    let capability = match (self.current_module.as_deref(), decl.name.as_str()) {
                        (Some("std.core.marker"), "Copy") => Some(LangItem::Copy),
                        (Some("std.core.marker"), "Send") => Some(LangItem::Send),
                        (Some("std.core.marker"), "Sync") => Some(LangItem::Sync),
                        (Some("std.thread"), "ThreadJob") => Some(LangItem::ThreadJob),
                        _ => None,
                    };
                    if let Some(capability) = capability {
                        self.symbols.set_lang_item(symbol, capability);
                    }
                }
            }
            TopLevelDecl::Extern(decl) => {
                // Intrinsics / FFI block members are the module surface (exportable).
                for member in &decl.members {
                    let symbol = self.define_with_visibility(
                        scope,
                        &member.name,
                        SymbolKind::ExternFunc,
                        member.span,
                        Visibility::Public,
                    );
                    if arandu_parser::AbiKind::from_abi_str(&decl.abi)
                        == arandu_parser::AbiKind::AranduIntrinsic
                        && let Some(marker) = arandu_middle::IntrinsicKind::from_name(&member.name)
                            .and_then(|kind| kind.declaration_marker())
                            .or_else(|| {
                                arandu_middle::intrinsics::SimdIntrinsic::from_name(&member.name)
                                    .map(|_| arandu_middle::symbol_table::LangItem::SimdIntrinsic)
                            })
                        && let Some(symbol) = symbol
                    {
                        self.symbols.set_lang_item(symbol, marker);
                    }
                }
            }
            TopLevelDecl::Submodule(submod) => {
                let mod_sym = self.define_with_visibility(
                    scope,
                    &submod.name,
                    SymbolKind::Module,
                    submod.span,
                    submod.visibility,
                );
                let sub_scope = self.symbols.new_scope(scope);
                if let Some(id) = mod_sym {
                    self.symbols.module_scopes.insert(id, sub_scope);
                }
                for &inner_id in &submod.decls {
                    let inner_decl = self.pool.decl(inner_id);
                    self.collect_top_level(sub_scope, inner_decl);
                }
                let symbols_in_subscope: Vec<arandu_middle::SymbolId> =
                    self.symbols.scope(sub_scope).symbols().to_vec();
                for sym_id in symbols_in_subscope {
                    let sym = self.symbols.get(sym_id);
                    let member_name = sym.name.clone();
                    self.symbols
                        .module_members
                        .insert((submod.name.clone(), member_name.clone()), sym_id);
                    if let Some(ref cur_mod) = self.current_module {
                        let qualified: smol_str::SmolStr =
                            format!("{cur_mod}.{}", submod.name).into();
                        self.symbols
                            .module_members
                            .insert((qualified, member_name), sym_id);
                    }
                }
            }
            TopLevelDecl::Error(_) => {}
        }
    }
}
