USB_PID=df11
FLASH_ADDRESS=0x08000000
# SRAM_ADDRESS=0x24000000
# QSPI_ADDRESS=0x90040000
APP_BIN=target/ansuz.bin
rm $APP_BIN
cargo objcopy --release -- -O binary $APP_BIN
dfu-util -a 0 -s $FLASH_ADDRESS:leave -D $APP_BIN -d ,0483:$USB_PID
