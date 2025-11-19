use kulta::crd::rollout::Rollout;
use kube::CustomResourceExt;

fn main() {
    // Generate CRD and print as JSON (kubectl accepts JSON)
    let crd = Rollout::crd();
    let json = serde_json::to_string_pretty(&crd).expect("Failed to serialize CRD to JSON");
    println!("{}", json);
}
