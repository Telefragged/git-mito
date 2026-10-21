# Runs tests/scenario.sh as an unprivileged user on a scratch btrfs disk,
# once without and once with the `user_subvol_rm_allowed` mount option.
{
  testers,
  git,
  git-mito,
  btrfs-progs,
}:
testers.runNixOSTest {
  name = "git-mito";

  nodes.machine = {
    virtualisation.emptyDiskImages = [ 1024 ];
    environment.systemPackages = [
      git
      git-mito
      btrfs-progs
    ];
    users.users.alice.isNormalUser = true;
  };

  testScript = ''
    scenario = "bash ${../tests/scenario.sh}"

    machine.wait_for_unit("multi-user.target")
    machine.succeed(
        "mkfs.btrfs /dev/vdb",
        "mkdir -p /mnt/btrfs",
        "mount /dev/vdb /mnt/btrfs",
        "install -d -o alice /mnt/btrfs/alice",
    )

    with subtest("without user_subvol_rm_allowed, deletes fall back to rm -rf"):
        out = machine.succeed(f"su - alice -c '{scenario} /mnt/btrfs/alice/plain' 2>&1")
        print(out)
        assert "`user_subvol_rm_allowed` mount option" in out, "expected the slow-delete warning"

    with subtest("with user_subvol_rm_allowed, subvolumes are deleted directly"):
        machine.succeed("mount -o remount,user_subvol_rm_allowed /mnt/btrfs")
        out = machine.succeed(f"su - alice -c '{scenario} /mnt/btrfs/alice/fast' 2>&1")
        print(out)
        assert "warning: deleting" not in out, "unexpected slow-delete warning"
  '';
}
