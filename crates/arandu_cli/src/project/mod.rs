//! Project orchestration, scaffolding, environment diagnosis, and loading.

pub mod doctor;
pub mod load;
pub mod lock;
pub mod module_map;
pub mod scaffold;
pub mod vcs;

pub use doctor::cmd_doctor;
pub use load::{
    ARANDU_VERSION, BackendChoice, ProjectContext, ProjectFlags, TargetKind, load_project,
    parse_project_flags,
};
pub use scaffold::{cmd_init, cmd_new, parse_scaffold_options};
