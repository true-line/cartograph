#!/bin/bash
# One-time setup of the locked evaluation account on a dedicated eval host
# (#258, ADR-0082). Run on that host by an account that can sudo.
# Usage: eval-runner-account-setup.sh '<ssh-ed25519 public key line>'
# It creates a hidden standard (non-admin) account `cartograph-eval` with a
# 0700 home, discards a random password (password login is never used), and
# installs one SSH key that can only run the scoring command. It installs no
# software and changes nothing else.
set -euo pipefail
U=cartograph-eval
H=/Users/$U
PUB=${1:?public key line required}
# Exactly one key line: a newline or any other control character could add a
# second, unrestricted entry to authorized_keys.
KEY_RE='^ssh-ed25519 [A-Za-z0-9+/]{68}(=){0,2}( [[:print:]]{0,128})?$'
if [[ ! "$PUB" =~ $KEY_RE ]] || [[ "$PUB" == *[[:cntrl:]]* ]]; then
  echo "expected exactly one ssh-ed25519 public key line" >&2; exit 2
fi

if id "$U" >/dev/null 2>&1; then echo "account $U already exists; stopping" >&2; exit 1; fi

PW=$(openssl rand -base64 33)
sudo sysadminctl -addUser "$U" -fullName "Cartograph eval runner" -home "$H" -password "$PW"
unset PW
# Never an admin, hidden from the login window.
if dseditgroup -o checkmember -m "$U" admin >/dev/null 2>&1; then
  echo "$U is unexpectedly an admin; stopping" >&2; exit 1
fi
sudo dscl . create "/Users/$U" IsHidden 1
[ -d "$H" ] || sudo createhomedir -c -u "$U" >/dev/null
sudo chown "$U":staff "$H"
sudo chmod 700 "$H"
sudo -u "$U" mkdir -p -m 700 "$H/.ssh" "$H/store" "$H/bin"
printf 'command="%s/bin/eval-runner serve",restrict %s\n' "$H" "$PUB" \
  | sudo -u "$U" tee "$H/.ssh/authorized_keys" >/dev/null
sudo -u "$U" chmod 600 "$H/.ssh/authorized_keys"
# When Remote Login is limited to listed users, list this one too.
if dscl . read /Groups/com.apple.access_ssh >/dev/null 2>&1; then
  sudo dseditgroup -o edit -a "$U" -t user com.apple.access_ssh
fi
echo "created $U: home $(stat -f %Sp "$H"), admin=no, forced command installed"
