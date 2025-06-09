MEMORY
{
	FLASH       (RX)  : ORIGIN = 0x08000000, LENGTH = 128K
	DTCMRAM     (RWX) : ORIGIN = 0x20000000, LENGTH = 128K
	SRAM        (RWX) : ORIGIN = 0x24000000, LENGTH = 512K - 32K
	RAM_D2_DMA  (RWX) : ORIGIN = 0x30000000, LENGTH = 32K
	RAM_D2      (RWX) : ORIGIN = 0x30008000, LENGTH = 256K
	RAM_D3      (RWX) : ORIGIN = 0x38000000, LENGTH = 64K
	BACKUP_SRAM (RWX) : ORIGIN = 0x38800000, LENGTH = 4K
	ITCMRAM     (RWX) : ORIGIN = 0x00000000, LENGTH = 64K
	SDRAM       (RWX) : ORIGIN = 0xc0000000, LENGTH = 64M
	QSPIFLASH   (RX)  : ORIGIN = 0x90040000, LENGTH = 7936K
}

EXTERN(__RESET_VECTOR);
EXTERN(Reset);
ENTRY(Reset);

EXTERN(__EXCEPTIONS);

EXTERN(DefaultHandler);

PROVIDE(NonMaskableInt = DefaultHandler);
EXTERN(HardFaultTrampoline);
PROVIDE(MemoryManagement = DefaultHandler);
PROVIDE(BusFault = DefaultHandler);
PROVIDE(UsageFault = DefaultHandler);
PROVIDE(SecureFault = DefaultHandler);
PROVIDE(SVCall = DefaultHandler);
PROVIDE(DebugMonitor = DefaultHandler);
PROVIDE(PendSV = DefaultHandler);
PROVIDE(SysTick = DefaultHandler);

PROVIDE(DefaultHandler = DefaultHandler_);
PROVIDE(HardFault = HardFault_);

EXTERN(__INTERRUPTS);

PROVIDE(__pre_init = DefaultPreInit);

SECTIONS
{
  PROVIDE(_stack_start = ORIGIN(DTCMRAM) + LENGTH(DTCMRAM));

  .vector_table ORIGIN(SRAM) :
  {
    __vector_table = .;
    LONG(_stack_start & 0xFFFFFFF8);
    KEEP(*(.vector_table.reset_vector));
    __exceptions = .;
    KEEP(*(.vector_table.exceptions));
    __eexceptions = .;
    KEEP(*(.vector_table.interrupts));
  } > SRAM

  PROVIDE(_stext = ADDR(.vector_table) + SIZEOF(.vector_table));

  .text _stext :
  {
    __stext = .;
    *(.Reset);
    *(.text .text.*);
    *(.HardFaultTrampoline);
    *(.HardFault.*);
    . = ALIGN(4);
    __etext = .;
  } > SRAM

  .rodata : ALIGN(4)
  {
    . = ALIGN(4);
    __srodata = .;
    *(.rodata .rodata.*);
    . = ALIGN(4);
    __erodata = .;
  } > SRAM

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
  } > RAM_D2_DMA

  .data : ALIGN(4)
  {
    . = ALIGN(4);
    __sdata = .;
    *(.data .data.*);
    . = ALIGN(4);
  } > DTCMRAM AT > SRAM
  . = ALIGN(4);
  __edata = .;

  __sidata = LOADADDR(.data);

  .gnu.sgstubs : ALIGN(32)
  {
    . = ALIGN(32);
    __veneer_base = .;
    *(.gnu.sgstubs*)
    . = ALIGN(32);
  } > SRAM
  . = ALIGN(32);
  __veneer_limit = .;

  .bss (NOLOAD) : ALIGN(4)
  {
    . = ALIGN(4);
    __sbss = .;
    *(.bss .bss.*);
    *(COMMON);
    . = ALIGN(4);
  } > DTCMRAM
  . = ALIGN(4);
  __ebss = .;

  .dtcmram_bss (NOLOAD) :
  {
    . = ALIGN(4);
    _sdtcmram_bss = .;

    PROVIDE(__dtcmram_bss_start__ = _sdtcmram_bss);
    *(.dtcmram_bss)
    *(.dtcmram_bss*)
    . = ALIGN(4);
    _edtcmram_bss = .;

    PROVIDE(__dtcmram_bss_end__ = _edtcmram_bss);
  } > DTCMRAM

/*
  .sdram_bss (NOLOAD) :
  {
  	. = ALIGN(4);
  	_ssdram_bss = .;

  	PROVIDE(__sdram_bss_start = _ssdram_bss);
  	*(.sdram_bss)
  	*(.sdram_bss*)
  	. = ALIGN(4);
  	_esdram_bss = .;

  	PROVIDE(__sdram_bss_end = _esdram_bss);
  } > SDRAM

  .backup_sram (NOLOAD) :
  {
  	. = ALIGN(4);
  	_sbackup_sram = .;

  	PROVIDE(__backup_sram_start = _sbackup_sram);
  	*(.backup_sram)
  	*(.backup_sram*)
  	. = ALIGN(4);
  	_ebackup_sram = .;

  	PROVIDE(__backup_sram_end = _ebackup_sram);
  } > BACKUP_SRAM

  .qspiflash_text :
  {
  	. = ALIGN(4);
  	_sqspiflash_text = .;

  	PROVIDE(__qspiflash_text_start = _sqspiflash_text);
  	*(.qspiflash_text)
  	*(.qspiflash_text*)
  	. = ALIGN(4);
  	_eqspiflash_text = .;

  	PROVIDE(__qspiflash_text_end = _eqspiflash_text);
  } > QSPIFLASH

  .qspiflash_data :
  {
  	. = ALIGN(4);
  	_sqspiflash_data = .;

  	PROVIDE(__qspiflash_data_start = _sqspiflash_data);
  	*(.qspiflash_data)
  	*(.qspiflash_data*)
  	. = ALIGN(4);
  	_eqspiflash_data = .;

  	PROVIDE(__qspiflash_data_end = _eqspiflash_data);
  } > QSPIFLASH

  .qspiflash_bss (NOLOAD) :
  {
  	. = ALIGN(4);
  	_sqspiflash_bss = .;

  	PROVIDE(__qspiflash_bss_start = _sqspiflash_bss);
  	*(.qspiflash_bss)
  	*(.qspiflash_bss*)
  	. = ALIGN(4);
  	_eqspiflash_bss = .;

  	PROVIDE(__qspiflash_bss_end = _eqspiflash_bss);
  } > QSPIFLASH
*/

/*
  .heap (NOLOAD) :
  {
  	. = ALIGN(4);
  	PROVIDE(__heap_start__ = .);
  	PROVIDE(__heap_start = .);
  	KEEP(*(.heap))
  	. = ALIGN(4);
  	PROVIDE(__heap_end = .);
  	PROVIDE(__heap_end__ = .);
  } > RAM_D2
  PROVIDE(__sheap = __heap_start__);
*/

  .uninit (NOLOAD) : ALIGN(4)
  {
    . = ALIGN(4);
    __suninit = .;
    *(.uninit .uninit.*);
    . = ALIGN(4);
    __euninit = .;
  } > DTCMRAM
  PROVIDE(__sheap = __euninit);
  PROVIDE(_stack_end = __euninit);

  .got (NOLOAD) :
  {
    KEEP(*(.got .got.*));
  }

  /DISCARD/ :
  {
    *(.ARM.exidx);
    *(.ARM.exidx.*);
    *(.ARM.extab.*);
  }
}

/* Do not exceed this mark in the error messages below                                    | */
/* # Alignment checks */
ASSERT(ORIGIN(SRAM) % 4 == 0, "
ERROR(cortex-m-rt): the start of the SRAM region must be 4-byte aligned");

ASSERT(ORIGIN(DTCMRAM) % 4 == 0, "
ERROR(cortex-m-rt): the start of the RAM region must be 4-byte aligned");

ASSERT(__sdata % 4 == 0 && __edata % 4 == 0, "
BUG(cortex-m-rt): .data is not 4-byte aligned");

ASSERT(__sidata % 4 == 0, "
BUG(cortex-m-rt): the LMA of .data is not 4-byte aligned");

ASSERT(__sbss % 4 == 0 && __ebss % 4 == 0, "
BUG(cortex-m-rt): .bss is not 4-byte aligned");

ASSERT(__sheap % 4 == 0, "
BUG(cortex-m-rt): start of .heap is not 4-byte aligned");

ASSERT(_stack_start % 8 == 0, "
ERROR(cortex-m-rt): stack start address is not 8-byte aligned.
If you have set _stack_start, check it's set to an address which is a multiple of 8 bytes.
If you haven't, stack starts at the end of RAM by default. Check that both RAM
origin and length are set to multiples of 8 in the `memory.x` file.");

ASSERT(_stack_end % 4 == 0, "
ERROR(cortex-m-rt): end of stack is not 4-byte aligned");

ASSERT(_stack_start >= _stack_end, "
ERROR(cortex-m-rt): stack end address is not below stack start.");

/* # Position checks */

/* ## .vector_table
 *
 * If the *start* of exception vectors is not 8 bytes past the start of the
 * vector table, then we somehow did not place the reset vector, which should
 * live 4 bytes past the start of the vector table.
 */
ASSERT(__exceptions == ADDR(.vector_table) + 0x8, "
BUG(cortex-m-rt): the reset vector is missing");

ASSERT(__eexceptions == ADDR(.vector_table) + 0x40, "
BUG(cortex-m-rt): the exception vectors are missing");

ASSERT(SIZEOF(.vector_table) > 0x40, "
ERROR(cortex-m-rt): The interrupt vectors are missing.
Possible solutions, from most likely to less likely:
- Link to a svd2rust generated device crate
- Check that you actually use the device/hal/bsp crate in your code
- Disable the 'device' feature of cortex-m-rt to build a generic application (a dependency
may be enabling it)
- Supply the interrupt handlers yourself. Check the documentation for details.");

/* ## .text */
ASSERT(ADDR(.vector_table) + SIZEOF(.vector_table) <= _stext, "
ERROR(cortex-m-rt): The .text section can't be placed inside the .vector_table section
Set _stext to an address greater than the end of .vector_table (See output of `nm`)");

ASSERT(_stext > ORIGIN(SRAM) && _stext < ORIGIN(SRAM) + LENGTH(SRAM), "
ERROR(cortex-m-rt): The .text section must be placed inside the SRAM memory.
Set _stext to an address within the SRAM region.");

/* # Other checks */
ASSERT(SIZEOF(.got) == 0, "
ERROR(cortex-m-rt): .got section detected in the input object files
Dynamic relocations are not supported. If you are linking to C code compiled using
the 'cc' crate then modify your build script to compile the C code _without_
the -fPIC flag. See the documentation of the `cc::Build.pic` method for details.");
/* Do not exceed this mark in the error messages above                                    | */

/* Provides weak aliases (cf. PROVIDED) for device specific interrupt handlers */
/* This will usually be provided by a device crate generated using svd2rust (see `device.x`) */
INCLUDE device.x

ASSERT(SIZEOF(.vector_table) <= 0x400, "
There can't be more than 240 interrupt handlers. This may be a bug in
your device crate, or you may have registered more than 240 interrupt
handlers.");
