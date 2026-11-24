# `tabkit-labels` makes the labels of the forge repo match .github/labels.yml and .github/labels.extra.yml.
# A label in a file is created, or renamed from one of its aliases, then given the color and description of the file.
# With --prune, a label in neither file is deleted, so the committed set is the whole set.
{ pkgs }:
pkgs.writeShellApplication {
  name = "tabkit-labels";
  runtimeInputs = [
    pkgs.gh
    pkgs.yq-go
    pkgs.jq
  ];
  text = ''
    prune=false
    dry=false
    for arg in "$@"; do
      case $arg in
      --prune) prune=true ;;
      --dry-run) dry=true ;;
      *)
        echo "Use: tabkit-labels [--prune] [--dry-run]" >&2
        exit 2
        ;;
      esac
    done
    files=(.github/labels.yml)
    [ -f .github/labels.extra.yml ] && files+=(.github/labels.extra.yml)
    # shellcheck disable=SC2016 # $item is a variable of yq, not of the shell.
    want=$(yq ea -o=json '. as $item ireduce ([]; . + $item)' "''${files[@]}")
    have=$(gh label list --limit 1000 --json name --jq '[.[].name]')
    run() {
      echo "$*"
      if [ "$dry" = false ]; then "$@" >/dev/null; fi
    }
    kept=$(jq -n '[]')
    while IFS= read -r label; do
      name=$(jq -r .name <<<"$label")
      color=$(jq -r .color <<<"$label")
      description=$(jq -r '.description // ""' <<<"$label")
      if jq -e --arg n "$name" 'index($n) != null' <<<"$have" >/dev/null; then
        run gh label edit "$name" --color "$color" --description "$description"
      else
        alias=$(jq -r --argjson have "$have" '[(.aliases // [])[] | select(. as $a | $have | index($a))] | first // empty' <<<"$label")
        if [ -n "$alias" ]; then
          run gh label edit "$alias" --name "$name" --color "$color" --description "$description"
          kept=$(jq --arg a "$alias" '. + [$a]' <<<"$kept")
        else
          run gh label create "$name" --color "$color" --description "$description"
        fi
      fi
    done < <(jq -c '.[]' <<<"$want")
    if [ "$prune" = true ]; then
      jq -r --argjson want "$want" --argjson kept "$kept" '.[] | select(. as $n | ($want | map(.name) | index($n)) == null and ($kept | index($n)) == null)' <<<"$have" |
        while IFS= read -r name; do run gh label delete "$name" --yes; done
    fi
  '';
}
