use super::*;

install! { prepare(reg, _cx) [] => [ports, registry] {
    let mut registry = Registry::new();
    // one Ports for the contacts routes and for the registrations that need it
    let ports = steve_modules::accounts::ports();
    steve_modules::accounts::register(&mut registry);
    steve_modules::contacts::changes::register(&mut registry, &ports);
    steve_modules::contacts::merge::register(&mut registry, &ports);
}}

install! { routes(reg, _cx) [mut module_routes] => [module_routes] {
    module_routes = module_routes.merge(steve_modules::accounts::routes());
}}
