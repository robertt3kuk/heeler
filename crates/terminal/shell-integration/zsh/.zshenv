# Staffy shell integration: ZDOTDIR shim (zsh).
#
# Staffy starts zsh with ZDOTDIR pointing at this directory, so zsh reads this file first.
# It puts the user's own ZDOTDIR back, runs the user's real ~/.zshenv, and arranges for the
# integration to be loaded on the first prompt, after every other startup file has run.
# The user's .zprofile/.zshrc/.zlogin are then read by zsh itself from the restored ZDOTDIR,
# exactly once, and nothing in them can tell the difference.

# 1. Restore the user's ZDOTDIR (unset when they had none).
if [[ -n ${STAFFY_ORIG_ZDOTDIR-} ]]; then
  ZDOTDIR=$STAFFY_ORIG_ZDOTDIR
else
  unset ZDOTDIR
fi
unset STAFFY_ORIG_ZDOTDIR

# 2. Run the user's .zshenv in place of this file.
if [[ -f ${ZDOTDIR:-$HOME}/.zshenv ]]; then
  source "${ZDOTDIR:-$HOME}/.zshenv"
fi

# 3. Load the integration from the first precmd hook: every startup file has run by then,
#    so the prompt-framework hooks are already registered and ours go behind them.
if [[ -o interactive && -r ${STAFFY_SHELL_INTEGRATION_DIR-}/zsh/staffy-integration.zsh ]]; then
  _staffy_load_integration() {
    add-zsh-hook -d precmd _staffy_load_integration
    unfunction _staffy_load_integration
    source "$STAFFY_SHELL_INTEGRATION_DIR/zsh/staffy-integration.zsh"
  }
  autoload -Uz add-zsh-hook
  add-zsh-hook precmd _staffy_load_integration
fi
