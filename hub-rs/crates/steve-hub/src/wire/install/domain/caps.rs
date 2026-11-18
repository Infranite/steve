use super::*;

install! { install(reg, cx) [mut registry, vault] => [caps, library, registry, vault] {
    let Env { steve_home, data_dir, .. } = cx.clone();
    // 能力: MCP servers, skills and remote agents, defined once and assigned per Bot; the
    // secrets in the vault. The Hub is the MCP client of the library's servers (their stdio ones
    // start in the sandbox a Node's runs get), and a session's brief says what its tools cover.
    let caps = steve_modules::caps::Capabilities::new(vault.clone());
    let library = crate::wire::library_mcp::LibraryMcp::new(
        caps.clone(),
        Some(steve_node::Sandbox::open(
            steve_home,
            &data_dir.join("node"),
        )),
    );
    registry.prompt_context(Box::new(crate::wire::tool_service::CatalogContext {
        library: library.clone(),
    }));
}}

install! { routes(reg, _cx) [caps, mut routes] => [caps, routes] {
    routes = routes.merge(steve_modules::caps::routes(caps.clone()));
}}
