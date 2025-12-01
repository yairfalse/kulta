use kube::CustomResourceExt;
use kulta::crd::rollout::Rollout;

fn main() {
    match serde_json::to_string_pretty(&Rollout::crd()) {
        Ok(crd_yaml) => print!("{}", crd_yaml),
        Err(e) => {
            eprintln!("Error serializing CRD: {}", e);
            std::process::exit(1);
        }
    }
}
