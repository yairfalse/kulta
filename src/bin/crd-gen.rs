use kube::CustomResourceExt;
use kulta::crd::rollout::Rollout;

fn main() {
    print!("{}", serde_json::to_string_pretty(&Rollout::crd()).unwrap());
}
