use super::*;

install! { install(reg, cx) [mut registry, vault] => [caps, library, registry, vault] {
    // 能力: MCP servers, skills and remote agents, defined once and assigned per Bot; the
    // secrets in the vault. The Hub is the MCP client of HTTP/SSE servers and delegates stdio
    // MCP servers to the colocated Node process with one-shot secret grants.
    let caps = steve_modules::caps::Capabilities::new(vault.clone());
    let library = crate::wire::library_mcp::LibraryMcp::new(
        caps.clone(),
        cx.workspaces.clone(),
        cx.secrets.clone(),
        cx.bus.caches().clone(),
    );
    registry.prompt_context(Box::new(crate::wire::tool_service::CatalogContext {
        library: library.clone(),
    }));
}}

install! { routes(reg, _cx) [caps, mut routes] => [caps, routes] {
    routes = routes.merge(steve_modules::caps::routes(caps.clone()));
}}
