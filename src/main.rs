mod luau_ir;

use bevy::prelude::*;
use mluau::prelude::*;

fn main() -> AppExit {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_systems(Startup, setup_lua)
        .run()
}

fn setup_lua(mut commands: Commands) {
    let lua = Lua::new();

    let print_from_rust = lua
        .create_function(|_, msg: String| {
            info!("[Luau]: {}", msg);
            Ok(())
        })
        .unwrap();

    let globals = lua.globals();
    globals.set("info", print_from_rust).unwrap();

    lua.load(
        r#"
            info("Hello from Luau inside Bevy!")
        "#,
    )
    .exec()
    .unwrap();

    commands.write_message(AppExit::Success);
}
