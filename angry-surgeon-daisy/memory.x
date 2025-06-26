MEMORY
{
	FLASH     (RX)  : ORIGIN = 0x08000000, LENGTH = 128K
	DTCMRAM   (RWX) : ORIGIN = 0x20000000, LENGTH = 128K
	SRAM      (RWX) : ORIGIN = 0x24000000, LENGTH = 512K
	RAM_D2    (RWX) : ORIGIN = 0x30000000, LENGTH = 288K
    RAM_D3    (RWX) : ORIGIN = 0x38000000, LENGTH = 64K
	QSPIFLASH (RX)  : ORIGIN = 0x90040000, LENGTH = 7936K
}

/* REGION_ALIAS(FLASH, QSPIFLASH); */
REGION_ALIAS(RAM, SRAM);

SECTIONS
{
    .sram1_bss (NOLOAD) :
    {
        . = ALIGN(4);
        _ssram1_bss = .;

        PROVIDE(__sram1_bss_start__ = _sram1_bss);
        *(.sram1_bss)
        *(.sram1_bss*)
        . = ALIGN(4);
        _esram1_bss = .;

        PROVIDE(__sram1_bss_end__ = _esram1_bss);
    } > RAM_D2
}
