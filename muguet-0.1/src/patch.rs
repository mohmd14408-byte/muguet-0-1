// muguet - patch.rs
// Constantin Clerc (c22dev) - 0.1

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use wasm_encoder::reencode::{utils, Error, Reencode};
use wasm_encoder::ValType::I64;
use wasmparser::{KnownCustom, Name, Parser, Payload, TypeRef};

#[derive(Clone, Copy, PartialEq)]
enum Arg {
    Local0,
    TlsBase,

    Local1Replaced,
}

const HOOKS: &[(&str, &str, &[Arg])] = &[
    ("muguet_script", "GtaThread::Update(int)", &[Arg::Local0, Arg::TlsBase]),
    ("muguet_explosion", "CExplosionManager::AddExplosion(CExplosionManager::CExplosionArgs&, CProjectile*, bool)", &[Arg::Local0]),
    ("muguet_timewarp", "CTimeWarper::SetTargetTimeWarp(TW_Dir)", &[Arg::Local1Replaced]),
];

pub const VERSION: u32 = 3;

fn returns(args: &[Arg]) -> bool {
    args.contains(&Arg::Local1Replaced)
}

struct Patcher {
    imported_funcs: u32,
    types: u32,
    tls_global: u32,

    targets: HashMap<u32, usize>,
    exports: Vec<(String, u32)>,
    next_func: u32,
}

impl Patcher {
    fn shift(&self) -> u32 {
        HOOKS.len() as u32
    }
}

impl Reencode for Patcher {
    type Error = anyhow::Error;

    fn function_index(&mut self, f: u32) -> Result<u32, Error<Self::Error>> {
        Ok(if f >= self.imported_funcs { f + self.shift() } else { f })
    }

    fn parse_type_section(&mut self, types: &mut wasm_encoder::TypeSection, section: wasmparser::TypeSectionReader<'_>) -> Result<(), Error<Self::Error>> {
        utils::parse_type_section(self, types, section)?;

        for (_, _, args) in HOOKS {
            types.ty().function(vec![I64; args.len()], if returns(args) { vec![I64] } else { vec![] });
        }
        Ok(())
    }

    fn parse_import_section(&mut self, imports: &mut wasm_encoder::ImportSection, section: wasmparser::ImportSectionReader<'_>) -> Result<(), Error<Self::Error>> {
        utils::parse_import_section(self, imports, section)?;
        for (k, (name, _, _)) in HOOKS.iter().enumerate() {
            imports.import("env", name, wasm_encoder::EntityType::Function(self.types + k as u32));
        }
        Ok(())
    }

    fn parse_export_section(&mut self, exports: &mut wasm_encoder::ExportSection, section: wasmparser::ExportSectionReader<'_>) -> Result<(), Error<Self::Error>> {
        utils::parse_export_section(self, exports, section)?;
        for (name, f) in std::mem::take(&mut self.exports) {
            exports.export(&name, wasm_encoder::ExportKind::Func, f + self.shift());
        }
        Ok(())
    }

    fn parse_function_body(&mut self, code: &mut wasm_encoder::CodeSection, func: wasmparser::FunctionBody<'_>) -> Result<(), Error<Self::Error>> {
        let index = self.next_func;
        self.next_func += 1;
        let Some(&hook) = self.targets.get(&index) else {
            return utils::parse_function_body(self, code, func);
        };
        let mut f = self.new_function_with_parsed_locals(&func)?;
        use wasm_encoder::Instruction as I;
        for arg in HOOKS[hook].2 {
            match arg {
                Arg::Local0 => f.instruction(&I::LocalGet(0)),
                Arg::TlsBase => f.instruction(&I::GlobalGet(self.tls_global)),
                Arg::Local1Replaced => f.instruction(&I::LocalGet(1)).instruction(&I::I64ExtendI32U),
            };
        }
        f.instruction(&I::Call(self.imported_funcs + hook as u32));
        if returns(HOOKS[hook].2) {
            f.instruction(&I::I32WrapI64);
            f.instruction(&I::LocalSet(1));
        }
        let mut reader = func.get_operators_reader()?;
        while !reader.eof() {
            f.instruction(&self.parse_instruction(&mut reader)?);
        }
        code.function(&f);
        Ok(())
    }
}

pub fn patch(wasm: &[u8], wanted: &[(&str, &str)]) -> Result<Vec<u8>> {
    let (mut imported_funcs, mut types, mut tls_global) = (0u32, 0u32, None);
    let mut by_name: HashMap<String, u32> = HashMap::new();
    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::TypeSection(s) => types = s.count(),
            Payload::ImportSection(s) => {
                for imp in s.into_imports() {
                    if matches!(imp?.ty, TypeRef::Func(_)) {
                        imported_funcs += 1;
                    }
                }
            }
            Payload::CustomSection(c) => {
                if let KnownCustom::Name(names) = c.as_known() {
                    for sub in names {
                        match sub? {
                            Name::Function(map) => {
                                for n in map {
                                    let n = n?;
                                    by_name.entry(n.name.to_string()).or_insert(n.index);
                                }
                            }
                            Name::Global(map) => {
                                for n in map {
                                    let n = n?;
                                    if n.name == "__tls_base" { tls_global = Some(n.index); }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if by_name.contains_key(HOOKS[0].0) {
        bail!("game.wasm is already patched");
    }
    let mut targets = HashMap::new();
    for (k, (_, func, _)) in HOOKS.iter().enumerate() {
        let f = *by_name.get(*func).with_context(|| format!("{func} not found (no name section?)"))?;
        targets.insert(f, k);
    }
    let mut exports = Vec::new();
    for (export, func) in wanted {
        match by_name.get(*func) {
            Some(&i) => exports.push((format!("mp:{export}"), i)),
            None => eprintln!("[patch] {func} not found; multiplayer will skip it"),
        }
    }
    let mut p = Patcher {
        imported_funcs,
        types,
        tls_global: tls_global.unwrap_or(1),
        targets,
        exports,
        next_func: imported_funcs,
    };
    let mut module = wasm_encoder::Module::new();
    p.parse_core_module(&mut module, Parser::new(0), wasm).map_err(|e| anyhow::anyhow!("patch game.wasm: {e:?}"))?;
    Ok(module.finish())
}
