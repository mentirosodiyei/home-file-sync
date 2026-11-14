//! Escribe en la salida estándar la página que ve el móvil, con datos de ejemplo.
//! Lo usa scripts/capturas.sh. Uso: pagina_movil <es|ca|en> <equipo> <carpeta>
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let lang = sfs_core::i18n::Lang::from_tag(a.get(1).map(String::as_str).unwrap_or("es"));
    print!("{}", sfs_core::inbox::sample_page(lang, &a[3], &a[2]));
}
