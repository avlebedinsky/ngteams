# shellcheck shell=bash
# die: print and stop; github_download: one GitHub release asset; fetch_tool: a pinned tool from its release archive,
# unpacked only after the archive's sha256 matches the pin (packaging/release/pins.env). Sourced, not run.

die() {
  echo "error: $*" >&2
  exit 1
}

# github_download URL OUT: download URL to OUT. When URL is a github.com release asset
# (https://github.com/OWNER/REPO/releases/download/TAG/NAME) and that path keeps failing, the same file comes through
# the REST API's asset endpoint instead. The caller checks the file's sha256 either way. GH_TOKEN, when set,
# authenticates the API calls (unauthenticated ones share a 60 an hour limit per address).
github_download() {
  local url="$1" out="$2" repo tag name asset
  local -a auth=()
  if curl --fail --silent --show-error --location --retry 6 --retry-delay 10 --connect-timeout 30 "$url" -o "$out"
  then
    return 0
  fi
  [[ "$url" =~ ^https://github\.com/([^/]+/[^/]+)/releases/download/([^/]+)/([^/]+)$ ]] || return 1
  repo="${BASH_REMATCH[1]}" tag="${BASH_REMATCH[2]}" name="${BASH_REMATCH[3]}"
  command -v jq >/dev/null || die "$url failed, and the API fallback needs jq"
  echo "$url failed: trying GitHub's API for $repo $tag $name" >&2
  [[ -z "${GH_TOKEN:-}" ]] || auth=(-H "Authorization: Bearer $GH_TOKEN")
  asset="$(curl --fail --silent --show-error --location --retry 6 --retry-delay 10 ${auth[@]+"${auth[@]}"} \
    -H "Accept: application/vnd.github+json" "https://api.github.com/repos/$repo/releases/tags/$tag" |
    jq -r --arg name "$name" '.assets[] | select(.name == $name) | .url')"
  [[ "$asset" == https://api.github.com/* ]] || die "$repo $tag: no release asset $name"
  curl --fail --silent --show-error --location --retry 6 --retry-delay 10 ${auth[@]+"${auth[@]}"} \
    -H "Accept: application/octet-stream" "$asset" -o "$out"
}

# fetch_tool ROOT NAME VERSION URL SHA256 MEMBER: print the directory under ROOT holding NAME, the MEMBER of the release
# archive at URL, unpacking it there first when it is not there yet
fetch_tool() (
  local root="$1" name="$2" version="$3" url="$4" sum="$5" member="$6" dir work
  dir="$root/$name-$version-$(uname -m)"
  if [[ ! -x "$dir/$name" ]]; then
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    # errexit is off here when fetch_tool runs in a command substitution, so a failed download stops explicitly
    github_download "$url" "$work/archive" || die "$name $version: the download failed: $url"
    echo "$sum  $work/archive" | sha256sum --check --quiet || die "$name $version: the archive's sha256 does not match"
    mkdir -p "$work/unpacked" "$root"
    tar -xf "$work/archive" -C "$work/unpacked" "$member"
    rm -rf "$dir.partial"
    mkdir "$dir.partial"
    install -m 755 "$work/unpacked/$member" "$dir.partial/$name"
    rm -rf "$dir"
    mv "$dir.partial" "$dir"
  fi
  echo "$dir"
)
