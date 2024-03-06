Simple Kubernetes backups
===

This Operator is designed to provide simple backups of PVCs in StorageClasses
that implement snapshotting. It works by snapshotting selected PVCs, mounting
those snapshots in a job container, and backing them up to a restic repository.

Please don't use this, it's really not that well tested or built, and
definitely isn't finished.

CRDS
---

**BackupSchedule** - Defines what will get backed up (via a list of field and
label selectors), when (via an interval scheduling syntax), and where to (via a
secret reference describing the destination Restic repo). Also may configure
common cleanup tasks for the Restic repository, such as pruning and checking
(Not implemented yet).

**BackupSet** - Defines a set of *BackupJobs* which are grouped together in a
single run of a *BackupSchedule*. Currently it's not possible to create a
*BackupSet* manually.

**BackupJob** - Defines a specific workload or PVC to be backed up. Normally
created by a *BackupSchedule*, but may also be created manually to create a
snapshot of a workload or PVC.

Annotations
---
For workloads:
- ros.io/before-backup - Command will be run in the container(s) before
  starting snapshot
- ros.io/after-backup - Command will be run in the container(s) after snapshot
  has finished
- ros.io/backup-command - Command will be run within a container, and by
  default the stdout of the command will be


Building images
---
```bash
docker build -f worker/Dockerfile . -t ghcr.io/jess-sol/walle/worker:latest --push
docker build . -t ghcr.io/jess-sol/walle/operator:latest --push
```

Development
---
```bash
minikube start --addons volumesnapshots,csi-hostpath-driver
kubectl patch storageclass csi-hostpath-sc -p '{"metadata": {"annotations":{"storageclass.kubernetes.io/is-default-class":"true"}}}'
k get crd | grep ros.io | awk '{print $1}' | xargs k delete crd
cargo run --bin crdgen | k apply -f -

k apply -f example/workload.yml
k create secret generic restic-rest-config --from-env-file=example/restic-secret.env

RUST_LOG=info,walle=debug APP_CONFIG=example/config.yml cargo run
# Or with Helm
helm upgrade --install walle ./helm/charts/walle -f example/values.yml

k apply -f example/backup-job.yml
# Or
k apply -f example/backup-schedule.yml
```
