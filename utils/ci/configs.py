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
The build image's tag is the Dockerfile's SHA-256, twelve hex
digits, as utils/ci/.circleci/config.yml publishes it. The configs are
rendered on their way: a line naming __ZBWAKE_RELEASE__ is repeated for each
gen 3 release of platform/diamond3/zbwake/releases, as the zbwake job's step
for that release. A fork's pull
request (FORK=true) is rendered without the docker-hub context and the
executors' Docker Hub credentials, and never publishes the image: the image is
public, and a fork's code gets no secret.

    utils/ci/configs.py setup PARAMETERS CONFIG-LIST
        In the setup job: rewrites the parameters file the path-filtering orb
        wrote and the list of configs to merge.
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


def configs_for(parameters):
    """The configs to merge for the parameters set: COMMON and each
    parameter's; NONE when nothing is set."""
    selected = dict.fromkeys(config for name in CONFIGS if parameters.get(name) is True for config in CONFIGS[name])
    if not selected:
        return [NONE, COMMON]
    return [COMMON, *selected]


def render(configs, directory=RENDERED, fork=False):
    """Copies of the configs with their placeholders filled, in directory;
    for a fork, without a context or registry credentials."""
    with open(ZBWAKE_RELEASES) as f:
        releases = f.read().split()
    rendered = []
    for config in configs:
        with open(config) as f:
            text = "".join(line.replace("__ZBWAKE_RELEASE__", release)
                           for line in f for release in (releases if "__ZBWAKE_RELEASE__" in line else [""]))
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
    parameters["image"] = image_tag()
    with open(parameters_path, "w") as f:
        json.dump(parameters, f)
    configs = configs_for(parameters)
    with open(config_list_path, "w") as f:
        f.writelines(config + "\n" for config in render(configs, fork=fork))
    print(json.dumps(parameters, indent=1))
    print("configs:", *configs)


def validate():
    every = [{}] + [{name: True} for name in CONFIGS] + [{name: True for name in CONFIGS}]
    # A fork's pull request never publishes the image or releases.
    selections = [(parameters, False) for parameters in every] + [({"app": True}, True)]
    failed = False
    for parameters, fork in selections:
        configs = configs_for(parameters)
        rendered = render(configs, tempfile.mkdtemp(), fork)
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
        print(f"{status}: {' '.join(sorted(parameters)) or 'nothing'}{' (fork)' if fork else ''} -> {len(configs)} configs")
        if result.returncode:
            print(result.stderr or result.stdout)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    if sys.argv[1:2] == ["setup"] and len(sys.argv) == 4:
        setup(sys.argv[2], sys.argv[3])
    elif sys.argv[1:] == ["validate"]:
        validate()
    else:
        sys.exit(__doc__)
