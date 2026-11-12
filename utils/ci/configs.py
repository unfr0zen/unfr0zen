#!/usr/bin/env python3
"""The repository's CircleCI configs, one per subsystem with its jobs, and
which of them a pipeline merges.

The setup workflow's path filtering sets a parameter per workflow the push's
files call for: the build image's and the app's. The config holding a workflow
declares its parameter and gates the workflow on it. The app and release
workflows list every module's job, so the subsystem configs defining them are
merged too: that is the table below. A push to main runs the app workflow, as
main has no base to compare with: what it changed says nothing about what
reached it. A release tag runs the release workflow alone, with the image's.

A job runs only when its inputs changed. Each job of INPUTS has a key of its
inputs: the build image, its sources' git trees, and the keys of the modules
it takes, so a change reaches everything downstream of it. A module's job
caches its outputs under its key and a check (the tests, clippy, an app) a
mark that it passed; the setup job restores them all, and a job found there
is rendered as the cached job under its name, as is a module nothing left to
run takes. A release tag runs every job.

The pipeline parameter force names jobs to run whatever the cache holds, or all
of them, for a pipeline started by hand:

    curl -X POST https://circleci.com/api/v2/project/gh/unfr0zen/unfr0zen/pipeline \
        -H "Circle-Token: $CIRCLE_TOKEN" -H 'Content-Type: application/json' \
        -d '{"branch": "main", "parameters": {"force": "radio test"}}'

It reaches the keys as FORCE, with the pipeline's number as SALT: a job named
gets a key of this pipeline alone, and so does everything taking it, which
builds and passes again on what the job made. Nothing later finds those keys.
The build image's tag is the Dockerfile's SHA-256, twelve hex
digits, as utils/ci/.circleci/config.yml publishes it. The configs are
rendered on their way: a line naming __ZBWAKE_RELEASE__ is repeated for each
gen 3 release of platform/diamond3/zbwake/releases, as the zbwake job's step
for that release. A fork's pull
request (FORK=true) is rendered without the docker-hub context and the
executors' Docker Hub credentials, and never publishes the image: the image is
public, and a fork's code gets no secret.

    utils/ci/configs.py keys
        Writes every job's key to .work/keys.
    utils/ci/configs.py setup PARAMETERS CONFIG-LIST
        In the setup job, after the caches are restored: rewrites the
        parameters file the path-filtering orb wrote and the list of configs
        to merge.
    utils/ci/configs.py validate
        Merges every selection a pipeline can make (yq) and validates each
        with the circleci CLI.
"""
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile

COMMON = ".circleci/common.yml"
RENDERED = "/tmp/circleci-configs"
ZBWAKE_RELEASES = "platform/diamond3/zbwake/releases"
NONE = ".circleci/none.yml"
DOCKERFILE = "utils/ci/Dockerfile"
IMAGE = "unfr0zen/unfr0zen-build"

# The configs defining the app's jobs: each module's, then its own.
APP = [
    "platform/diamond3/.circleci/config.yml",
    "platform/diamond3/zbwake/.circleci/config.yml",
    "platform/j49/.circleci/config.yml",
    "platform/diamond/.circleci/config.yml",
    "platform/common/radio/.circleci/config.yml",
    "platform/common/doom/.circleci/config.yml",
    "platform/common/nlclient/.circleci/config.yml",
    "crates/nestlocal/.circleci/config.yml",
    "crates/.circleci/config.yml",
]

# parameter: the config holding its workflow and those defining the jobs it lists
CONFIGS = {
    "build-image": ["utils/ci/.circleci/config.yml"],
    "app": APP,
    "release": APP + [".circleci/release.yml"],
}


ZBWAKE_SOURCES = ["platform/diamond3/zbwake/build.sh", "platform/diamond3/zbwake/zbwake.c",
                  "platform/diamond3/zbwake/Kbuild"]

# module: its sources as git paths, the modules it takes, and whether it
# builds in the build image. Each is the job of that name.
MODULES = {
    "recovery": {"paths": ["platform/diamond3/bootstrap", "platform/diamond3/uboot"]},
    "j49": {"paths": ["platform/j49"]},
    "j49-kernel": {"paths": ["platform/j49/kernel.sh", "platform/j49/xloader.sh", "platform/j49/zbwake"],
                   "image": False},
    "diamond-xloader": {"paths": ["platform/diamond"], "image": False},
    "zbwake": {"paths": ZBWAKE_SOURCES + [ZBWAKE_RELEASES]},
    "radio": {"paths": ["platform/common/radio"]},
    "thread": {"paths": ["platform/common/radio/thread"]},
    "doom": {"paths": ["platform/common/doom"]},
    # Its job also runs the screen's host tests.
    "nlclient": {"paths": ["platform/common/nlclient/src", "platform/common/nlclient/tests"]},
    # The radio module brings nestlocal.sh and the Makefile nestlocal reads;
    # nestlocal.sh also builds the wake pattern and local cloud libraries the
    # app installs. Its job builds unfr0zen-apply as well, the patch engine on
    # unfr0zen-core.
    "nestlocal": {"paths": ["crates/nestlocal", "crates/unfr0zen-apply", "crates/unfr0zen-core", "platform/common/wakefilter",
                            "platform/common/localcloud", "Cargo.toml", "Cargo.lock"],
                  "needs": ["radio", "zbwake", "j49-kernel"]},
}

# The checks: the jobs that build nothing a later one takes. All compile the
# workspace, which embeds every module and files of platform/ and assets/.
PLATFORMS = ["linux-x86_64", "linux-arm64", "linux-riscv64", "windows-x86_64", "windows-arm64", "macos"]
CHECK = {"paths": ["crates", "platform", "assets", "Cargo.toml", "Cargo.lock", ".config", "utils/make_macos_app.sh"],
         "needs": list(MODULES)}
CHECKS = dict.fromkeys(["test", "clippy"] + ["app-" + platform for platform in PLATFORMS], CHECK)

# The jobs the app workflow lists, the ones the setup job looks for in the cache.
APP_JOBS = list(MODULES) + ["test", "clippy", "app-linux-x86_64", "app-windows-x86_64", "app-macos"]


def releases():
    with open(ZBWAKE_RELEASES) as f:
        return f.read().split()


def inputs():
    """INPUTS: every key's inputs, the modules', the checks' and each zbwake
    release's, which the zbwake job caches one by one."""
    return MODULES | CHECKS | {"zbwake-" + release: {"paths": ZBWAKE_SOURCES, "of": "zbwake"}
                               for release in releases()}


def key(name, table, keys):
    """The job's key: a hash of its name and image, its sources' git ids and
    its modules' keys, and of SALT for a job FORCE names."""
    if name not in keys:
        job = table[name]
        force = os.environ.get("FORCE", "").split()
        lines = [f"{name} {image_tag() if job.get('image', True) else ''}"]
        lines += [subprocess.run(["git", "rev-parse", f"HEAD:{path}"], check=True, capture_output=True,
                                 text=True).stdout.strip() for path in job["paths"]]
        lines += [key(need, table, keys) for need in job.get("needs", [])]
        if "all" in force or name in force or job.get("of") in force:
            lines.append("force " + os.environ.get("SALT", ""))
        keys[name] = hashlib.sha256("".join(line + "\n" for line in lines).encode()).hexdigest()
    return keys[name]


def write_keys():
    table, keys = inputs(), {}
    unknown = set(os.environ.get("FORCE", "").split()) - set(table) - {"all"}
    if unknown:
        sys.exit(f"force names no job: {' '.join(sorted(unknown))}; the jobs are {' '.join(MODULES | CHECKS)}")
    os.makedirs(".work/keys", exist_ok=True)
    os.makedirs(".work/cache", exist_ok=True)
    for name in table:
        with open(".work/keys/" + name, "w") as f:
            f.write(key(name, table, keys) + "\n")
        print(name, keys[name])


def cached_jobs():
    """The jobs with nothing to do: the checks whose mark and the modules
    whose tarball the setup job restored, and the modules missing from the
    cache that no job left to run takes."""
    table = inputs()
    found = {name for name in APP_JOBS
             if os.path.exists(f".work/passed/{name}" if name in CHECKS else f".work/cache/{name}.tar")}
    run = {name for name in APP_JOBS if name in CHECKS and name not in found}
    pending = list(run)
    while pending:
        for need in table[pending.pop()].get("needs", []):
            if need not in found and need not in run:
                run.add(need)
                pending.append(need)
    return set(APP_JOBS) - run


def configs_for(parameters):
    """The configs to merge for the parameters set: COMMON and each
    parameter's; NONE when nothing is set."""
    selected = dict.fromkeys(config for name in CONFIGS if parameters.get(name) is True for config in CONFIGS[name])
    if not selected:
        return [NONE, COMMON]
    return [COMMON, *selected]


def render(configs, directory=RENDERED, fork=False, cached=()):
    """Copies of the configs with their placeholders filled, in directory;
    for a fork, without a context or registry credentials. A workflow's entry
    for a job of cached becomes the cached job under the same name, keeping
    what the entry requires and what requires it."""
    rendered = []
    for config in configs:
        with open(config) as f:
            text = "".join(line.replace("__ZBWAKE_RELEASE__", release)
                           for line in f for release in (releases() if "__ZBWAKE_RELEASE__" in line else [""]))
        jobs, workflows, entries = text.partition("\nworkflows:\n")
        for name in cached:
            entries = re.sub(rf"^( +)- (?:{name}:?|[a-z0-9-]+:\n\1    name: {name})$",
                             rf"\1- cached:\n\1    name: {name}", entries, flags=re.M)
        text = jobs + workflows + entries
        if fork:
            text = re.sub(r"^( *)context: docker-hub$", r"\1context: []", text, flags=re.M)
            text = re.sub(r"^( *)auth:\n\1  username: \$DOCKERHUB_USER\n\1  password: \$DOCKERHUB_TOKEN\n", "",
                          text, flags=re.M)
            if re.search(r"^ *(context: (?!\[\]$)|auth:)", text, flags=re.M):
                sys.exit(f"{config}: a context or credentials left in a fork's config")
        path = os.path.join(directory, config.replace("/", "-"))
        os.makedirs(directory, exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
        rendered.append(path)
    return rendered


def image_tag():
    with open(DOCKERFILE, "rb") as dockerfile:
        return IMAGE + ":" + hashlib.sha256(dockerfile.read()).hexdigest()[:12]


def setup(parameters_path, config_list_path):
    with open(parameters_path) as f:
        parameters = json.load(f)
    if os.environ.get("CIRCLE_TAG"):
        parameters = {"build-image": parameters.get("build-image", False), "release": True}
    elif os.environ.get("CIRCLE_BRANCH") == "main":
        # The image only when its Dockerfile changed: this one publishes it.
        parameters = {"app": True, "build-image": parameters.get("build-image", False)}
    fork = os.environ.get("FORK") == "true"
    if fork:
        parameters["build-image"] = False
    cached = sorted(cached_jobs()) if parameters.get("app") else []
    parameters["image"] = image_tag()
    with open(parameters_path, "w") as f:
        json.dump(parameters, f)
    configs = configs_for(parameters)
    with open(config_list_path, "w") as f:
        f.writelines(config + "\n" for config in render(configs, fork=fork, cached=cached))
    print(json.dumps(parameters, indent=1))
    print("configs:", *configs)
    print("cached:", *cached)


def validate():
    every = [{}] + [{name: True} for name in CONFIGS] + [{name: True for name in CONFIGS}]
    # A fork's pull request never publishes the image or releases.
    # The app with every job to run, with none, and with the checks alone.
    selections = [(parameters, False, []) for parameters in every] + [({"app": True}, True, [])] \
        + [({"app": True}, False, APP_JOBS), ({"app": True}, False, list(MODULES))]
    failed = False
    # The setup job and the firmware command restore what the table names.
    with open(".circleci/config.yml") as f:
        setup_config = f.read()
    with open(COMMON) as f:
        common = f.read()
    for name in APP_JOBS:
        restore = f'{"passed" if name in CHECKS else "module"}-{name}-v2-{{{{ checksum ".work/keys/{name}" }}}}'
        for config, text in ((".circleci/config.yml", setup_config), (COMMON, common)):
            if restore not in text and (name in MODULES or text is setup_config):
                failed = True
                print(f"FAILED: {config} does not restore {restore}")
    for parameters, fork, cached in selections:
        configs = configs_for(parameters)
        rendered = render(configs, tempfile.mkdtemp(), fork, cached)
        entries = sum(open(path).read().count("- cached:\n") for path in rendered)
        if entries != len(cached):
            failed = True
            print(f"FAILED: {entries} entries rendered as cached for {len(cached)} jobs")
        merged = subprocess.run(["yq", "eval-all", "explode(.) | . as $item ireduce ({}; . * $item )", *rendered],
                                check=True, capture_output=True, text=True).stdout
        with tempfile.NamedTemporaryFile("w", suffix=".yml", delete=False) as f:
            f.write(merged)
        values = {**{name: True for name in parameters}, "image": IMAGE + ":000000000000"}
        result = subprocess.run(["circleci", "config", "process", "--pipeline-parameters", json.dumps(values), f.name],
                                capture_output=True, text=True)
        os.unlink(f.name)
        status = "ok" if result.returncode == 0 else "FAILED"
        failed |= result.returncode != 0
        print(f"{status}: {' '.join(sorted(parameters)) or 'nothing'}{' (fork)' if fork else ''}"
              f"{f' ({len(cached)} cached)' if cached else ''} -> {len(configs)} configs")
        if result.returncode:
            print(result.stderr or result.stdout)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    if sys.argv[1:] == ["keys"]:
        write_keys()
    elif sys.argv[1:2] == ["setup"] and len(sys.argv) == 4:
        setup(sys.argv[2], sys.argv[3])
    elif sys.argv[1:] == ["validate"]:
        validate()
    else:
        sys.exit(__doc__)
