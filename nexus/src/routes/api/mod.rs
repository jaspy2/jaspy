// UI-facing API. Everything under /api/v1 is the surface the web admin UI
// talks to, and the single mount that will be put behind authentication later.
// Machine-to-machine consumers (cli tools, the trap-handler subcommand, prometheus,
// switchmaster, weathermap) keep using the stable /dev/* routes.
pub mod v1;
