import json, os, pathlib, sys
home = pathlib.Path(os.environ["SG44_HOME"])
mode = os.environ["SG44_MODE"]
command = sys.argv[1]
with (home / "trace").open("a") as out:
    out.write(command + "\n")
if command == "build":
    commit, version = sys.argv[2], sys.argv[4]
    candidate = home / "releases" / f"{version}-local-{commit[:7]}"
    candidate.mkdir(parents=True)
    (candidate / "partial").write_text("kept unless ownership was acknowledged")
    if mode != "foreign-failure":
        print(f"fixture build {version} from {commit} into {candidate}", flush=True)
    if mode in ["owned-failure", "foreign-failure"]:
        sys.exit(2)
    (candidate / "bin").mkdir()
    (candidate / "web/dist").mkdir(parents=True)
    (candidate / "bin/steve-hub").write_text("")
    (candidate / "web/dist/index.html").write_text("")
    (candidate / "release.json").write_text(json.dumps({"version":version,"commit":commit}))
    sys.exit(0)
assert command == "deploy"
sys.exit(1 if mode == "deploy-refused" else 0)
