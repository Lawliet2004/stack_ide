use mlua::prelude::*;

/// Applies sandbox restrictions to a Lua state.
/// Removes dangerous libraries (io, os, package, debug) and overrides print().
pub fn apply_sandbox(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();

    // Remove dangerous standard libraries
    globals.set("io", LuaValue::Nil)?;
    globals.set("os", LuaValue::Nil)?;
    globals.set("package", LuaValue::Nil)?;
    globals.set("debug", LuaValue::Nil)?;
    globals.set("load", LuaValue::Nil)?;
    globals.set("loadstring", LuaValue::Nil)?;
    globals.set("dofile", LuaValue::Nil)?;

    // Override print() to redirect to stderr with plugin name prefix
    // This will be done per-plugin in api.rs when we know the plugin name

    Ok(())
}

/// Sets an instruction count hook to prevent infinite loops.
/// limit: max instructions before interruption (e.g., 1_000_000).
/// Uses the builder pattern: `HookTriggers::new().every_nth_instruction(limit)`.
pub fn set_instruction_limit(lua: &Lua, limit: u32) -> LuaResult<()> {
    lua.set_hook(
        mlua::HookTriggers::new().every_nth_instruction(limit),
        move |_lua, _debug| Err(LuaError::external("Instruction limit exceeded")),
    );

    Ok(())
}
