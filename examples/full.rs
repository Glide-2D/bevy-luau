use bevy::prelude::*;

fn main() -> AppExit {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(bevy_luau::ScriptingPlugin)
        .run()
}
