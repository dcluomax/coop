# Coop quickstart demo — recorded with charmbracelet/vhs.
#
#   Regenerate (needs a coopd serving on 127.0.0.1:9788):
#     cargo build --workspace --bins
#     coopd --data-dir /tmp/coop-demo-data serve --addr 127.0.0.1:9788 &
#     vhs docs/demo/coop-demo.tape
#
# Every visible line is a live `coop` CLI command and its real output —
# no fabrication.

Output docs/demo/coop-demo.gif

Set Shell "bash"
Set FontSize 18
Set Width 1200
Set Height 700
Set Padding 22
Set Theme "Dracula"
Set TypingSpeed 30ms

# ---- hidden, non-blocking setup ----
Hide
Type "cd ~/coop"
Enter
Type 'export PATH="$PWD/target/debug:$PATH"'
Enter
Type "export COOP_PASSPHRASE=demo-pass API=http://127.0.0.1:9788"
Enter
Type "clear"
Enter
Show

# ---- the demo ----
Type "# 🐔 Coop — raise AI agents like hens on hardware you own"
Sleep 700ms
Enter
Sleep 600ms

Type "coop --api $API health"
Enter
Sleep 1200ms

Type "# seal a BYOK vault for your provider API keys"
Enter
Sleep 350ms
Type "coop --api $API vault init /tmp/coop-demo-data/vault.json"
Enter
Sleep 1300ms

Type "# define a hen from a YAML manifest, then hatch (boot) it"
Enter
Sleep 350ms
Type "coop --api $API hen create examples/aria.yaml"
Enter
Sleep 1100ms
Type "coop --api $API hen hatch local.coop/aria"
Enter
Sleep 1300ms

Type "# the hen is alive and IDLE, waiting for jobs"
Enter
Sleep 350ms
Type `coop --api $API hen list | grep -E '"id"|"state"'`
Enter
Sleep 1800ms

Type "coop --api $API farm"
Enter
Sleep 2600ms
