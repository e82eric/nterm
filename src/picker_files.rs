use crate::picker_config::FileOptions;

pub(crate) fn resolve_roots(options: &mut FileOptions) -> Result<(), String> {
    if options.roots.is_empty() {
        options.roots.push(
            std::env::current_dir()
                .map_err(|error| format!("Unable to determine filesystem picker root: {error}"))?,
        );
    }
    for root in &mut options.roots {
        *root =
            std::path::absolute(&*root).map_err(|error| format!("{}: {error}", root.display()))?;
    }
    Ok(())
}
