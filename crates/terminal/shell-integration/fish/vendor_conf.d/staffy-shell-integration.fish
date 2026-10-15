# Staffy shell integration for fish: OSC 133 prompt marks and OSC 7 working directory.
#
#   A  prompt start          B  prompt end / input start
#   C  command output start  D;<status>  command finished
#
# Staffy prepends its shell-integration directory to XDG_DATA_DIRS, so fish loads this file
# from fish/vendor_conf.d before the user's config.fish. It can also be sourced by hand:
#   source "$STAFFY_SHELL_INTEGRATION_DIR/fish/vendor_conf.d/staffy-shell-integration.fish"

# Hand XDG_DATA_DIRS back to the user's value, for this shell and everything it starts.
if set -q STAFFY_ORIG_XDG_DATA_DIRS
    set -gx XDG_DATA_DIRS $STAFFY_ORIG_XDG_DATA_DIRS
else if set -q STAFFY_XDG_DATA_DIRS_UNSET
    set -e XDG_DATA_DIRS
end
set -e STAFFY_ORIG_XDG_DATA_DIRS
set -e STAFFY_XDG_DATA_DIRS_UNSET

status is-interactive; or return 0
set -q __staffy_integration_loaded; and return 0
set -g __staffy_integration_loaded 1

# fish 4.0+ marks prompts itself (feature flag mark-prompt): A, C and D; the prompt-end mark B
# and an OSC 7 on every prompt since 4.4. Only what fish does not emit is added here.
set -g __staffy_native_marks 0
set -g __staffy_native_complete 0
if status test-feature mark-prompt
    set __staffy_native_marks 1
    if string match -qr -- '^(4\.([4-9]|[1-9][0-9]+)|([5-9]|[1-9][0-9]+))(\.|$|-)' $version
        set __staffy_native_complete 1
    end
end

function __staffy_osc7
    set -l path_encoded $PWD
    if string match -qr -- '[^A-Za-z0-9/._~-]' $PWD
        # Percent-encode byte by byte.
        set path_encoded (string escape --style=url -- $PWD)
    end
    printf '\e]7;file://%s%s\a' $hostname $path_encoded
end

function __staffy_wrap_prompt
    # Wrap fish_prompt (never replace it): the original keeps its name and output, B follows it.
    # Also re-wraps when the user redefines fish_prompt during the session.
    if functions fish_prompt | string match -q -- '*__staffy_orig_fish_prompt*'
        return
    end
    functions -c fish_prompt __staffy_orig_fish_prompt
    function fish_prompt
        __staffy_orig_fish_prompt $argv
        printf '\e]133;B\a'
    end
end

function __staffy_on_prompt --on-event fish_prompt
    if test $__staffy_native_complete -eq 0
        __staffy_osc7
    end
    if test $__staffy_native_marks -eq 0
        printf '\e]133;A\a'
    end
    if test $__staffy_native_complete -eq 0
        __staffy_wrap_prompt
    end
end

if test $__staffy_native_marks -eq 0
    function __staffy_on_preexec --on-event fish_preexec
        printf '\e]133;C\a'
    end

    function __staffy_on_postexec --on-event fish_postexec
        printf '\e]133;D;%s\a' $status
    end
end
