USB_PID=df11
FLASH_ADDRESS=0x08000000
BIN=target/app.bin
rm $BIN
cargo objcopy --release -- -O binary -S $BIN
dfu-util -a 0 -s $FLASH_ADDRESS:leave -D $BIN -d ,0483:$BIN

# USB_PID=df11
# QSPI_ADDRESS=0x90040000
# BIN=target/app.bin
# rm $BIN
# cargo objcopy --release -- -O binary -S $BIN
# dfu-util -a 0 -s $QSPI_ADDRESS:leave -D $BIN -d ,0483:$BIN
