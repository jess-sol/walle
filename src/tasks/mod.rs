use kube::{
    core::GroupVersionKind,
    discovery::ApiResource,
    runtime::events::{Recorder, Reporter},
    Client, Resource,
};

use crate::WALLE;

pub mod backup;
pub mod schedule;

/// Cache some expensive kubernetes operations so they're easily accessible throughout operator.
#[derive(Clone)]
pub struct KubeManager {
    client: Client,
    pub snapshot_ar: ApiResource,
    reporter: Reporter,
}

impl KubeManager {
    pub async fn new(client: Client) -> Result<Self, kube::Error> {
        // TODO - Support v1 and v1beta1
        // let apigroup =
        //     kube::discovery::pinned_group(&client, &GroupVersion::gv("snapshot.storage.k8s.io", "v1"))
        //         .await
        //         .unwrap();
        // println!("{:?}", apigroup.recommended_kind("VolumeSnapshot"));
        let gvk = GroupVersionKind::gvk("snapshot.storage.k8s.io", "v1", "VolumeSnapshot");
        let (snapshot_ar, _caps) = kube::discovery::pinned_kind(&client, &gvk).await?;

        Ok(Self { client, snapshot_ar, reporter: format!("{WALLE}-controller").into() })
    }

    pub fn client(&self) -> Client {
        self.client.clone()
    }

    pub fn recorder(&self, resource: &impl Resource<DynamicType = ()>) -> Recorder {
        Recorder::new(self.client(), self.reporter.clone(), resource.object_ref(&()))
    }
}
