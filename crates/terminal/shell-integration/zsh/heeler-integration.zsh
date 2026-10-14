# Heeler shell integration for zsh: OSC 133 prompt marks and OSC 7 working directory.
#
#   A  prompt start          B  prompt end / input start
#   C  command output start  D;<status>  command finished
#
# Loaded automatically by Heeler's ZDOTDIR shim (zsh/.zshenv); it can also be sourced by hand
# from ~/.zshrc: source "$HEELER_SHELL_INTEGRATION_DIR/zsh/heeler-integration.zsh"
# Hooks go through precmd_functions/preexec_functions; no user function is replaced.

[[ -o interactive ]] || return 0
(( ${+_heeler_integration_loaded} )) && return 0
builtin typeset -g _heeler_integration_loaded=1
# 0 = no command since the last prompt, 1 = a command is running.
builtin typeset -gi _heeler_command_running=0

_heeler_osc7() {
  builtin local path_encoded=$PWD
  if [[ $PWD == *[^A-Za-z0-9/._~-]* ]]; then
    # Percent-encode byte by byte (the C locale makes ${PWD[i]} a byte).
    builtin local LC_ALL=C ch
    builtin local -i i
    path_encoded=
    for (( i = 1; i <= ${#PWD}; ++i )); do
      ch=${PWD[i]}
      case $ch in
        [A-Za-z0-9/._~-]) path_encoded+=$ch ;;
        *) path_encoded+=$(builtin printf '%%%02X' "'$ch") ;;
      esac
    done
  fi
  builtin printf '\e]7;file://%s%s\a' "$HOST" "$path_encoded"
}

_heeler_precmd() {
  builtin local exit_status=$?
  if (( _heeler_command_running )); then
    builtin printf '\e]133;D;%d\a' "$exit_status"
    _heeler_command_running=0
  fi
  _heeler_osc7
  # Prompt frameworks rewrite PS1 in their own precmd hooks, which run before this one, so
  # the marks are (re)added here; a PS1 that already carries OSC 133 is left alone.
  if [[ $PS1 != *$'\e]133;'* ]]; then
    PS1=$'%{\e]133;A\a%}'$PS1$'%{\e]133;B\a%}'
  fi
  return $exit_status
}

_heeler_preexec() {
  _heeler_command_running=1
  builtin printf '\e]133;C\a'
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd _heeler_precmd
add-zsh-hook preexec _heeler_preexec
# This is the first prompt: precmd hooks were snapshotted before we registered, so run ours
# now. (A prompt framework that rewrites PS1 later in this very cycle hides the marks once.)
_heeler_precmd
