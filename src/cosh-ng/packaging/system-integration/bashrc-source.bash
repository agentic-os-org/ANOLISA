# Requires enabled POSIX special readonly/exit; builtin-table rewriting is unsupported.
case $- in
  *i*)
    if [[ $- != *r* && -r /usr/libexec/anolisa/cosh-ng/cosh-integration.bash ]] && {
      if ( if POSIXLY_CORRECT=1 && [[ :$SHELLOPTS: == *:posix:* ]]; then
          if \readonly -f builtin 2>/dev/null; then \exit 0
          else case $? in 1) \exit 2 ;; *) \exit 1 ;; esac; fi
        else ((0)); fi ) 2>/dev/null; then ((0))
      else case $? in 2) ((1)) ;; *) ((0)) ;; esac; fi
    }; then
      \builtin source /usr/libexec/anolisa/cosh-ng/cosh-integration.bash || \builtin :
    fi
    ;;
esac
