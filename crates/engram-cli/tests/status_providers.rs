//! The provider status surface: `doctor --json` and `config check --json`
//! both carry one `providers` entry per provider with its role, key source,
//! live state, balance, severity, what degrades, and a fix. No test here
//! reaches the network: `config check` only runs with every key stripped
//! (nothing to probe), and the keyed case runs `doctor`, which never probes.

mod contract_support;

use contract_support::Sandbox;
use serde_json::Value;

const FIELDS: &[&str] = &[
    "name",
    "provider",
    "role",
    "required",
    "configured",
    "live",
    "http_status",
    "balance",
    "severity",
    "degrades",
    "fix",
];

fn providers(v: &Value) -> &Vec<Value> {
    v["data"]["providers"]
        .as_array()
        .unwrap_or_else(|| panic!("no providers array: {v}"))
}

fn by_name<'a>(v: &'a Value, name: &str) -> &'a Value {
    providers(v)
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("no provider {name}: {v}"))
}

#[test]
fn with_every_key_unset_both_surfaces_name_each_provider_and_its_fix() {
    let sb = Sandbox::new();
    for args in [
        &["doctor", "--json"][..],
        &["config", "check", "--json"][..],
    ] {
        let out = sb.cmd().args(args).output().expect("engram");
        assert!(out.status.success(), "{args:?}");
        let v = Sandbox::json(&out);
        let names: Vec<&str> = providers(&v)
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["gemini", "openrouter", "cohere", "typesafe"],
            "{args:?}"
        );
        for p in providers(&v) {
            for f in FIELDS {
                assert!(p.get(*f).is_some(), "{args:?} {}: missing {f}", p["name"]);
            }
            assert_eq!(p["configured"]["source"], "unset", "{args:?}");
            assert_eq!(p["live"], "not_probed", "{args:?}");
            assert!(
                p["fix"]
                    .as_str()
                    .is_some_and(|f| f.contains("engram config set keys.")),
                "{args:?} {p}"
            );
        }
        assert_eq!(by_name(&v, "gemini")["severity"], "error", "{args:?}");
        assert_eq!(by_name(&v, "gemini")["required"], true, "{args:?}");
        let steps = v["data"]["next_steps"].as_array().expect("next_steps");
        assert!(
            steps[0].as_str().unwrap().starts_with("gemini (error): "),
            "{args:?} {steps:?}"
        );
        assert_eq!(v["data"]["ok"], false, "{args:?}");
    }
}

#[test]
fn doctor_masks_configured_keys_and_sends_the_agent_to_config_check() {
    let sb = Sandbox::new();
    let gemini = "AIzaFAKESTATUSKEY0123456789abcdef";
    let openrouter = "sk-or-v1-FAKESTATUSKEY0123456789abcdef";
    let out = sb
        .cmd()
        .env("GEMINI_API_KEY", gemini)
        .env(
            "OPENROUTER_API_KEY",
            "TYPESAFE_API_KEY",
            "GEMINI_API_KEY_FALLBACKS",
            "ENGRAM_JUDGE_STUB",
            "ENGRAM_JUDGE_GROUNDING",
            "ENGRAM_JUDGE_ADJUDICATE",
            "ENGRAM_JUDGE_DEDUPE",
            "ENGRAM_JUDGE_LOG",
            openrouter,
        )
        .args(["doctor", "--json"])
        .output()
        .expect("doctor");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains(gemini) && !text.contains(openrouter),
        "{text}"
    );
    let v = Sandbox::json(&out);
    for (name, key) in [("gemini", gemini), ("openrouter", openrouter)] {
        let p = by_name(&v, name);
        assert_eq!(p["configured"]["source"], "env");
        assert_eq!(
            p["configured"]["value"],
            format!("{}...{}", &key[..6], &key[key.len() - 4..])
        );
        // Doctor never dials the network: presence is reported as unchecked.
        assert_eq!(p["live"], "not_probed");
        assert_eq!(p["severity"], "unchecked");
        assert!(p["fix"]
            .as_str()
            .unwrap()
            .contains("engram config check --json"));
    }
    assert_eq!(by_name(&v, "gemini")["balance"], "not_exposed_by_provider");
    // OpenRouter's balance is only known after a probe.
    assert_eq!(by_name(&v, "openrouter")["balance"], Value::Null);
}
