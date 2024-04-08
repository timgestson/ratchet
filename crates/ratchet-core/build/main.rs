mod binary;
mod concat;
mod gemm;
mod norm;
mod reindex;
mod unary;

use anyhow::Context as anyhowCtx;
use binary::BinaryOp;
use concat::ConcatOp;
use gemm::Gemm;
use norm::NormOp;
use reindex::ReindexOp;
use unary::UnaryOp;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use tera::Tera;

/// # Generate
///
/// This trait is used to generate the kernels for the different operations.
pub trait Generate {
    fn generate(renderer: &mut KernelRenderer) -> anyhow::Result<()>;
}

#[derive(strum_macros::EnumIter, Debug)]
pub enum KernelElement {
    Scalar,
    Vec2,
    Vec4,
}

impl std::fmt::Display for KernelElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            KernelElement::Scalar => "scalar",
            KernelElement::Vec2 => "vec2",
            KernelElement::Vec4 => "vec4",
        };
        write!(f, "{}", s)
    }
}

pub enum WgslDType {
    F32,
}

impl std::fmt::Display for WgslDType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WgslDType::F32 => write!(f, "f32"),
        }
    }
}

impl KernelElement {
    pub fn as_wgsl(&self, dtype: WgslDType) -> String {
        match self {
            KernelElement::Scalar => dtype.to_string(),
            KernelElement::Vec2 => format!("vec2<{}>", dtype),
            KernelElement::Vec4 => format!("vec4<{}>", dtype),
        }
    }

    pub fn as_size(&self) -> usize {
        match self {
            KernelElement::Scalar => 1,
            KernelElement::Vec2 => 2,
            KernelElement::Vec4 => 4,
        }
    }
}

#[derive(Debug)]
pub struct KernelRenderer {
    tera: Tera,
    dest_path: PathBuf,
    templates_path: PathBuf,
}

impl Default for KernelRenderer {
    fn default() -> Self {
        let base_path = Path::new(env!("CARGO_MANIFEST_DIR"));
        KernelRenderer {
            tera: Tera::default(),
            dest_path: base_path.join("kernels").join("generated"),
            templates_path: base_path.join("kernel-templates"),
        }
    }
}

impl KernelRenderer {
    fn generate(&mut self) -> anyhow::Result<()> {
        UnaryOp::generate(self)?;
        BinaryOp::generate(self)?;
        ReindexOp::generate(self)?;
        NormOp::generate(self)?;
        Gemm::generate(self)?;
        ConcatOp::generate(self)?;
        Ok(())
    }
}

fn embed_kernels() -> anyhow::Result<()> {
    let out_dir = env!("CARGO_MANIFEST_DIR").to_string() + "/src";
    let mut file = std::fs::File::create(Path::new(&out_dir).join("kernels.rs")).context(
        "Failed to create `src/kernels.rs`. Make sure you have `src` directory in your project.",
    )?;
    writeln!(
        &file,
        "// This file is generated by build.rs. Do not edit it manually."
    )?;
    writeln!(&mut file, "use std::collections::HashMap;")?;
    writeln!(&mut file, "use lazy_static::lazy_static;")?;
    writeln!(&mut file, "lazy_static! {{")?;
    writeln!(
        &mut file,
        "pub static ref KERNELS: HashMap<&'static str, &'static str> = {{"
    )?;
    writeln!(&mut file, "    let mut m = HashMap::new();")?;
    for entry in
        globwalk::glob(env!("CARGO_MANIFEST_DIR").to_string() + "/kernels/**.wgsl")?.flatten()
    {
        let path = entry.path();
        let name = path.file_stem().unwrap().to_str().unwrap();

        let diff = pathdiff::diff_paths(path, Path::new(out_dir.as_str()))
            .ok_or(anyhow::format_err!("Failed to get path diff"))?;

        writeln!(
            &mut file,
            "    m.insert(\"{}\", include_str!(r\"{}\"));",
            name,
            diff.display()
        )?;
    }
    writeln!(&mut file, "    m")?;
    writeln!(&mut file, "}};")?;
    writeln!(&mut file, "}}")?;

    Ok(())
}

fn main() {
    let mut generator = KernelRenderer::default();
    generator.generate().unwrap();
    embed_kernels().unwrap();
    if let Err(e) = Command::new("cargo").args(["fmt"]).status() {
        eprintln!("Failed to execute `cargo fmt`: {}", e);
    }
}