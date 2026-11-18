use super::*;

install! { prepare(reg, cx) [mut registry] => [registry] {
    let Env { store, yolo_forced, .. } = cx.clone();
    // YOLO: its line in `/汇报` and the `yolo` draft target (“开 YOLO，前端项目除外”)
    registry.digest_notes(Box::new(steve_core::yolo::digest::Notes {
        store: store.clone(),
    }));
    steve_core::yolo::draft::register(&mut registry, yolo_forced);
}}
