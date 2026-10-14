# Heeler shell integration for bash: OSC 133 prompt marks and OSC 7 working directory.
#
#   A  prompt start          B  prompt end / input start
#   C  command output start  D;<status>  command finished
#
# Heeler starts bash as `bash --rcfile <this file> ...`, so an interactive bash sources this
# file instead of ~/.bashrc. The trampoline below replays the startup sequence bash would have
# run (for a login shell, /etc/profile and the first of ~/.bash_profile, ~/.bash_login,
# ~/.profile; otherwise ~/.bashrc or the user's own --rcfile) and only then installs the
# hooks, so the user's files run exactly once and before the integration. (`--posix` + $ENV,
# the other usual route, is not used: bash 3.2, macOS's /bin/bash, ignores it for startup.)
# The file can also be sourced by hand from ~/.bashrc:
#   source "$HEELER_SHELL_INTEGRATION_DIR/bash/heeler.bash"
#
# The user's files are sourced here at the top level, never inside a function, so their
# `declare`s behave as in a normal startup.

case $- in
  *i*) ;;
  *) return 0 2>/dev/null || exit 0 ;;
esac

if [ -n "${HEELER_BASH_INJECT+set}" ]; then
  # Flags of the original command line: login, norc, noprofile.
  __heeler_inject=" $HEELER_BASH_INJECT "
  __heeler_rcfile=${HEELER_BASH_RCFILE-}
  unset HEELER_BASH_INJECT HEELER_BASH_RCFILE

  case $__heeler_inject in
    *" login "*)
      case $__heeler_inject in
        *" noprofile "*) ;;
        *)
          [ -r /etc/profile ] && . /etc/profile
          for __heeler_file in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
            if [ -r "$__heeler_file" ]; then
              . "$__heeler_file"
              break
            fi
          done
          ;;
      esac
      ;;
    *)
      case $__heeler_inject in
        *" norc "*) ;;
        *)
          if [ -n "$__heeler_rcfile" ]; then
            [ -r "$__heeler_rcfile" ] && . "$__heeler_rcfile"
          else
            [ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"
          fi
          ;;
      esac
      ;;
  esac
  unset __heeler_inject __heeler_rcfile __heeler_file
fi

# Hooks are installed once per shell; the rest of the file only defines functions.
if [ -n "${__heeler_integration_loaded+set}" ]; then
  return 0
fi
__heeler_integration_loaded=1
# 0 = no command since the last prompt, 1 = a command is running.
__heeler_command_running=0
# DEBUG-trap mode only (bash < 4.4): 1 between the end of PROMPT_COMMAND and the next command.
__heeler_armed=0

__heeler_osc7() {
  local path_encoded=$PWD
  case $PWD in
    *[!A-Za-z0-9/._~-]*)
      # Percent-encode byte by byte (the C locale makes ${PWD:i:1} a byte).
      local LC_ALL=C ch i code hex
      path_encoded=
      for ((i = 0; i < ${#PWD}; i++)); do
        ch=${PWD:i:1}
        case $ch in
          [A-Za-z0-9/._~-]) path_encoded+=$ch ;;
          *)
            # printf reads a byte >= 0x80 as a negative char; keep the low byte.
            printf -v code '%d' "'$ch"
            printf -v hex '%%%02X' $((code & 255))
            path_encoded+=$hex
            ;;
        esac
      done
      ;;
  esac
  printf '\e]7;file://%s%s\a' "${HOSTNAME-}" "$path_encoded"
}

# First in PROMPT_COMMAND: the previous command is over, so report it. $? is passed on
# untouched to whatever PROMPT_COMMAND entries follow.
__heeler_precmd() {
  local exit_status=$?
  if [ "$__heeler_command_running" = 1 ]; then
    printf '\e]133;D;%s\a' "$exit_status"
    __heeler_command_running=0
  fi
  __heeler_osc7
  return "$exit_status"
}

# Last in PROMPT_COMMAND: prompt frameworks rebuild PS1 in their own entries, so the marks are
# (re)added here; a PS1 that already carries OSC 133 is left alone.
__heeler_postprompt() {
  local exit_status=$?
  case $PS1 in
    *'\e]133;'*) ;;
    *) PS1='\[\e]133;A\a\]'$PS1'\[\e]133;B\a\]' ;;
  esac
  __heeler_armed=1
  return "$exit_status"
}

# bash < 4.4 has no PS0: the DEBUG trap fires before every simple command, and the first one
# after PROMPT_COMMAND ran is the user's command.
__heeler_preexec() {
  if [ "$__heeler_armed" = 1 ] && [ -z "${COMP_POINT-}" ] && [ -z "${READLINE_POINT-}" ]; then
    __heeler_armed=0
    __heeler_command_running=1
    printf '\e]133;C\a'
  fi
}

__heeler_install() {
  # PROMPT_COMMAND: an array from bash 5.1, a string before.
  case $(declare -p PROMPT_COMMAND 2>/dev/null) in
    "declare -a"*)
      PROMPT_COMMAND=(__heeler_precmd "${PROMPT_COMMAND[@]}" __heeler_postprompt)
      ;;
    *)
      PROMPT_COMMAND=$'__heeler_precmd\n'${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}'__heeler_postprompt'
      ;;
  esac

  if ((BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4))); then
    # PS0 is expanded just before the command runs, in the shell itself, so the arithmetic in
    # the substring offset (of length 0, so it expands to nothing) records that a command started.
    PS0=${PS0-}'\e]133;C\a${__heeler_command_running:$((__heeler_command_running=1)):0}'
  fi
}

# Splits the output of `trap -p DEBUG` (`trap -- 'body' DEBUG`) into its body.
__heeler_trap_body() {
  eval "set -- $1"
  __heeler_previous_body=${3-}
}

__heeler_install
unset -f __heeler_install

# bash < 4.4 has no PS0: install the DEBUG trap. It is set here, not inside a function: bash
# 3.2 undoes a trap set in a function when the function returns. A trap the user already has
# is kept and runs after ours; `trap -p` inside $(...) sees no traps in bash 3.2 (command
# substitutions reset them), so its output goes through a file.
if ((BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4))); then
  __heeler_trap_file=$(mktemp "${TMPDIR:-/tmp}/heeler-trap.XXXXXX" 2>/dev/null) || __heeler_trap_file=
  __heeler_previous_body=
  if [ -n "$__heeler_trap_file" ]; then
    trap -p DEBUG >"$__heeler_trap_file"
    __heeler_trap_body "$(<"$__heeler_trap_file")"
    rm -f "$__heeler_trap_file"
  fi
  if [ -n "$__heeler_previous_body" ]; then
    eval "__heeler_previous_debug_trap() { $__heeler_previous_body
}"
    trap '__heeler_preexec; __heeler_previous_debug_trap' DEBUG
  else
    trap '__heeler_preexec' DEBUG
  fi
  unset __heeler_trap_file __heeler_previous_body
fi
unset -f __heeler_trap_body
