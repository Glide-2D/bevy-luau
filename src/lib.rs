#![expect(
    unsafe_code,
    reason = "Unsafe code is needed to work with dynamic components"
)]

pub mod bridge;
pub mod commands;
pub mod loading;
pub mod pool;
pub mod query;
pub mod runtime;
pub mod schema;
pub mod systems;
pub mod types;

use bevy::prelude::*;
use loading::load_scripts;
use pool::EngineStringPool;
use runtime::ScriptingRuntime;
use schema::SchemaRegistry;
use systems::{lua_startup_system, lua_update_system};

pub struct ScriptingPlugin;

impl Plugin for ScriptingPlugin {
    fn build(&self, app: &mut App) {
        app.init_non_send::<ScriptingRuntime>()
            .init_non_send::<EngineStringPool>()
            .init_resource::<SchemaRegistry>()
            .add_systems(Startup, (load_scripts, lua_startup_system).chain())
            .add_systems(Update, lua_update_system);
    }
}
