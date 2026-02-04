use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(etsi_codec)");
    println!("cargo:rerun-if-env-changed=ETSI_CODEC_ENABLE");

    let enable = env::var("ETSI_CODEC_ENABLE").ok().as_deref() == Some("1");
    if !enable {
        println!("cargo:warning=ETSI codec build disabled (set ETSI_CODEC_ENABLE=1 to enable)");
        return;
    }

    println!("cargo:rerun-if-env-changed=ETSI_CODEC_DIR");
    println!("cargo:rerun-if-env-changed=ETSI_CODEC_SRC");

    let codec_dir = env::var("ETSI_CODEC_DIR")
        .ok()
        .or_else(|| env::var("ETSI_CODEC_SRC").ok())
        .and_then(|p| find_c_code_dir(Path::new(&p)))
        .or_else(|| find_c_code_dir(Path::new("src/etsi/C-CODE")))
        .or_else(|| find_c_code_dir(Path::new("src/etsi")))
        .or_else(|| find_c_code_dir(Path::new("third_party/etsi_codec")));

    let Some(codec_dir) = codec_dir else {
        println!("cargo:warning=ETSI codec not found; traffic channel coding will run in passthrough mode. Set ETSI_CODEC_DIR or place sources in src/etsi/C-CODE.");
        return;
    };

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap()).join("etsi_codec");
    if let Err(err) = prepare_codec_sources(&codec_dir, &out_dir) {
        println!("cargo:warning=Failed to prepare ETSI codec sources: {err}");
        return;
    }

    let mut build = cc::Build::new();
    build
        .include(&out_dir)
        .flag_if_supported("-std=c99");

    let c_files = [
        "scod_tet.c",
        "sdec_tet.c",
        "sub_sc_d.c",
        "sub_dsp.c",
        "fbas_tet.c",
        "fexp_tet.c",
        "fmat_tet.c",
        "tetra_op.c",
        "ccod_tet.c",
        "cdec_tet.c",
        "sub_cc.c",
        "sub_cd.c",
    ];

    for file in c_files {
        build.file(out_dir.join(file));
    }

    build.compile("etsi_codec");

    println!("cargo:rustc-cfg=etsi_codec");
}

fn find_c_code_dir(root: &Path) -> Option<PathBuf> {
    if !root.exists() {
        return None;
    }
    if root.join("C-CODE").is_dir() {
        return Some(root.join("C-CODE"));
    }
    if root.join("c-code").is_dir() {
        return Some(root.join("c-code"));
    }
    if root.file_name().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("c-code")).unwrap_or(false) {
        return Some(root.to_path_buf());
    }
    None
}

fn prepare_codec_sources(src_dir: &Path, out_dir: &Path) -> io::Result<()> {
    if !out_dir.exists() {
        fs::create_dir_all(out_dir)?;
    }

    let files = [
        "scod_tet.c",
        "sdec_tet.c",
        "sub_sc_d.c",
        "sub_dsp.c",
        "fbas_tet.c",
        "fexp_tet.c",
        "fmat_tet.c",
        "tetra_op.c",
        "ccod_tet.c",
        "cdec_tet.c",
        "sub_cc.c",
        "sub_cd.c",
        "channel.h",
        "source.h",
        "arrays.tab",
        "const.tab",
        "clsp_334.tab",
        "ener_qua.tab",
        "grid.tab",
        "inv_sqrt.tab",
        "lag_wind.tab",
        "log2.tab",
        "pow2.tab",
        "window.tab",
    ];

    for name in files {
        let src = find_file_case_insensitive(src_dir, name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("missing {name} in {src_dir:?}")))?;
        let dst = out_dir.join(name);
        fs::copy(src, dst)?;
    }

    patch_typedefs(&out_dir.join("channel.h"))?;
    patch_typedefs(&out_dir.join("source.h"))?;
    patch_make_static_tables(&out_dir.join("const.tab"))?;
    patch_make_static_tables(&out_dir.join("arrays.tab"))?;
    patch_make_static_globals(&out_dir.join("scod_tet.c"))?;
    patch_make_static_globals(&out_dir.join("sdec_tet.c"))?;
    patch_make_static_symbols(&out_dir.join("sub_cc.c"), &["Channel_Encoding"])?;
    patch_make_static_symbols(&out_dir.join("sub_cd.c"), &["Channel_Decoding"])?;

    Ok(())
}

fn find_file_case_insensitive(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.exists() {
        return Some(direct);
    }
    let upper = dir.join(name.to_ascii_uppercase());
    if upper.exists() {
        return Some(upper);
    }
    let lower = dir.join(name.to_ascii_lowercase());
    if lower.exists() {
        return Some(lower);
    }

    let target = name.to_ascii_lowercase();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            if file_name.to_string_lossy().to_ascii_lowercase() == target {
                return Some(entry.path());
            }
        }
    }
    None
}

fn patch_typedefs(path: &Path) -> io::Result<()> {
    let contents = fs::read_to_string(path)?;
    let replacement = "#include <stdint.h>\ntypedef int16_t Word16;\ntypedef int32_t Word32;\ntypedef int32_t Flag;\n";

    let replaced = contents
        .replace(
            "typedef short Word16;\r\ntypedef long  Word32;\r\ntypedef int   Flag;\r\n",
            replacement,
        )
        .replace(
            "typedef short Word16;\ntypedef long  Word32;\ntypedef int   Flag;\n",
            replacement,
        );

    if replaced == contents {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("failed to patch typedefs in {}", path.display()),
        ));
    }

    fs::write(path, replaced)?;
    Ok(())
}

fn patch_make_static_tables(path: &Path) -> io::Result<()> {
    let contents = fs::read_to_string(path)?;
    let mut out = String::new();
    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("static") {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let check = strip_const_prefix(trimmed);
        if check.starts_with("Word")
            || check.starts_with("short")
            || check.starts_with("int")
            || check.starts_with("long")
        {
            let indent = &line[..line.len() - trimmed.len()];
            out.push_str(indent);
            out.push_str("static ");
            out.push_str(trimmed);
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    fs::write(path, out)?;
    Ok(())
}

fn patch_make_static_globals(path: &Path) -> io::Result<()> {
    let contents = fs::read_to_string(path)?;
    let mut out = String::new();
    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.starts_with("static") {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if is_global_decl_line(trimmed) {
            let indent = &line[..line.len() - trimmed.len()];
            out.push_str(indent);
            out.push_str("static ");
            out.push_str(trimmed);
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    fs::write(path, out)?;
    Ok(())
}

fn patch_make_static_symbols(path: &Path, allow: &[&str]) -> io::Result<()> {
    let contents = fs::read_to_string(path)?;
    let mut out = String::new();
    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.starts_with("static") {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if is_function_def_line(trimmed) {
            let name = function_name(trimmed).unwrap_or("");
            if allow.iter().any(|v| *v == name) {
                out.push_str(line);
                out.push('\n');
                continue;
            }
            let indent = &line[..line.len() - trimmed.len()];
            out.push_str(indent);
            out.push_str("static ");
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        if is_global_decl_line(trimmed) {
            let indent = &line[..line.len() - trimmed.len()];
            out.push_str(indent);
            out.push_str("static ");
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    fs::write(path, out)?;
    Ok(())
}

fn is_global_decl_line(trimmed: &str) -> bool {
    let has_paren = trimmed.contains('(');
    let ends_semi = trimmed.ends_with(';');
    if has_paren || !ends_semi {
        return false;
    }
    let check = strip_const_prefix(trimmed);
    check.starts_with("Word") || check.starts_with("short") || check.starts_with("int") || check.starts_with("long")
}

fn is_function_def_line(trimmed: &str) -> bool {
    if trimmed.ends_with(';') {
        return false;
    }
    if !(trimmed.starts_with("Word") || trimmed.starts_with("void") || trimmed.starts_with("short") || trimmed.starts_with("int") || trimmed.starts_with("long")) {
        return false;
    }
    trimmed.contains('(') && trimmed.contains(')')
}

fn function_name(trimmed: &str) -> Option<&str> {
    let idx = trimmed.find('(')?;
    let before = trimmed[..idx].trim();
    before.split_whitespace().last()
}

fn strip_const_prefix(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix("const") {
        let rest_trim = rest.trim_start();
        if rest_trim.len() != rest.len() {
            return rest_trim;
        }
    }
    line
}
