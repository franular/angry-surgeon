USB_PID=df11
FLASH_ADDRESS=0x08000000
BOOT_BIN=dsy_bootloader_v6_2-intdfu-2000ms.bin
dfu-util -a 0 -s $FLASH_ADDRESS:leave -D $BOOT_BIN -d ,0483:$USB_PID
