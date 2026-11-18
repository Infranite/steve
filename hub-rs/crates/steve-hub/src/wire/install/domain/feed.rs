use super::*;

install! { routes(reg, _cx) [mut module_routes] => [module_routes] {
    module_routes = module_routes.merge(crate::api::feed::routes());
}}
