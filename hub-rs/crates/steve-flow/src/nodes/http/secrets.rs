//! Secret eligibility is checked before any release; partial releases retain the original cleanup.
use super::*;
pub(super) struct Secrets {
    pub values: Vec<(String, Zeroizing<String>)>,
    pub uses: Vec<String>,
}
impl Http {
    pub(super) fn release_secrets(
        &self,
        cx: &mut RunCx<'_>,
        method: &str,
        url: &Url,
        refs: &[SecretRef],
    ) -> Result<Secrets, NodeOutcome> {
        // secrets: grant or your approval, else wait
        if !refs.is_empty() {
            let Some(vault) = &self.vault else {
                return Err(NodeOutcome::Fail(NO_VAULT.into()));
            };
            let (flow_name, owner) = flow_meta(cx);
            // your approval of this step; an `approve` node earlier on the path stands in for it
            // only for grantable items (its card names neither the secrets nor the host)
            let step_approved = cx.run.context["approved"][&cx.node.id] == json!(true);
            let path_approved = cx
                .run
                .context
                .get("approved_by")
                .is_some_and(|v| !v.is_null());
            let items = secret_items(refs);
            let command = format!("{method} {url}");
            let node_name = cx.node.name().to_string();
            let purpose = format!("流程「{flow_name}」·「{node_name}」");
            let (ws, flow_id, run_id) = (
                cx.run.workspace_id.clone(),
                cx.run.flow_id.clone(),
                cx.run.id.clone(),
            );
            let base = FlowSecretRequest {
                ws: &ws,
                flow_id: &flow_id,
                flow_version: cx.run.flow_version,
                flow_owner: &owner,
                run_id: &run_id,
                item_id: "",
                labels: &[],
                purpose: &purpose,
                command: &command,
                approved: step_approved,
            };
            let SecretAccess {
                need,
                grants,
                by_path,
            } = check_secret_access(vault, &base, &items, path_approved)?;
            if !need.is_empty() {
                let mut draft = format!("{command}\n注入：");
                for (item, labels) in &items {
                    draft.push_str(&format!("\n{item} ← {}", labels.join("、")));
                }
                cx.effects = Effects {
                    outcome: Some("need".into()),
                    label: Some(format!("等你批准：{node_name}")),
                    ..std::mem::take(&mut cx.effects)
                };
                return Err(NodeOutcome::Wait(Box::new(secret_gate(
                    cx, &flow_name, &need, draft, &grants,
                ))));
            }
            return release_values(vault, &base, &items, &by_path, step_approved);
        }
        Ok(Secrets {
            values: Vec::new(),
            uses: Vec::new(),
        })
    }
}
fn secret_items(refs: &[SecretRef]) -> Vec<(String, Vec<String>)> {
    let mut items: Vec<(String, Vec<String>)> = Vec::new();
    for r in refs {
        match items.iter_mut().find(|(i, _)| *i == r.item) {
            Some((_, f)) if !f.contains(&r.field) => f.push(r.field.clone()),
            Some(_) => {}
            None => items.push((r.item.clone(), vec![r.field.clone()])),
        }
    }

    items
}
struct SecretAccess {
    need: Vec<String>,
    grants: Vec<Grant>,
    by_path: Vec<bool>,
}
fn check_secret_access(
    vault: &Vault,
    base: &FlowSecretRequest<'_>,
    items: &[(String, Vec<String>)],
    path_approved: bool,
) -> Result<SecretAccess, NodeOutcome> {
    let mut need: Vec<String> = Vec::new();
    let mut grants: Vec<Grant> = Vec::new();
    let mut all_grantable = true;
    // per item: does the path's approval release it
    let mut by_path: Vec<bool> = Vec::new();
    for (item, labels) in items {
        let check = vault.check_flow_use(&FlowSecretRequest {
            item_id: item,
            labels,
            ..base.clone()
        });
        by_path.push(matches!(&check, Ok(c) if c.grantable && path_approved));
        match check {
            Ok(c) if c.allowed || (c.grantable && path_approved) => {}
            Ok(c) => {
                all_grantable &= c.grantable;
                grants.push(Grant::new("secret.use", "vault_item", item));
                need.push(c.title);
            }
            Err(e) => return Err(NodeOutcome::Fail(format!("机密「{item}」：{e}"))),
        }
    }
    if !all_grantable {
        grants.clear();
    }

    Ok(SecretAccess {
        need,
        grants,
        by_path,
    })
}

fn release_values(
    vault: &Vault,
    base: &FlowSecretRequest<'_>,
    items: &[(String, Vec<String>)],
    by_path: &[bool],
    step_approved: bool,
) -> Result<Secrets, NodeOutcome> {
    let mut values = Vec::new();
    let mut uses = Vec::new();
    for ((item, labels), path) in items.iter().zip(by_path) {
        match vault.use_for_flow(&FlowSecretRequest {
            item_id: item,
            labels,
            approved: step_approved || *path,
            ..base.clone()
        }) {
            Ok(FlowSecretOutcome::Released { use_id, values: vs }) => {
                uses.push(use_id);
                for (label, v) in vs {
                    values.push((
                        format!("{item}.{label}"),
                        Zeroizing::new(String::from_utf8_lossy(v.expose()).into_owned()),
                    ));
                }
            }
            Ok(FlowSecretOutcome::NeedsApproval { .. }) => {
                finish_uses(vault, &uses, false);
                return Err(NodeOutcome::Fail("机密的授权刚刚失效了，请重新运行".into()));
            }
            Err(e) => {
                finish_uses(vault, &uses, false);
                return Err(NodeOutcome::Fail(format!("机密「{item}」：{e}")));
            }
        }
    }
    Ok(Secrets { values, uses })
}
