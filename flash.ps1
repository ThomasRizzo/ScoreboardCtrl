cargo run -r
if($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

cp target/thumbv6m-none-eabi/release/hello_world_embassy.uf2 /mnt/chromeos/removable/RPI-RP2/
