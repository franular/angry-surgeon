USB_PID=df11
QSPI_ADDRESS=0x90040000
# APP_BIN=target/thumbv7em-none-eabihf/release/angry-surgeon-daisy
APP_BIN=target/ansuz.bin
rm $APP_BIN
cargo objcopy --release -- -O binary -S $APP_BIN
dfu-util -a 0 -s $QSPI_ADDRESS:leave -D $APP_BIN -d ,0483:$USB_PID
