This is a list of TODO, some might need options to have in config
analyze each carefully, if you see that there are conceputal gaps in the core and we need them first let me know.


## DO NEXT

### print whitespaces

i need command for displaying whitespace characters for debug purposes,
so each space becomes bullet displayed,
each new line becomes paragraph symbol,
tab arrow etc.

:whitespace

### proper seleciton with commandline

when i press : to go command mode and i have somehting selected,
then commands should operate on that selection only, unless it does not make sense then display error message,

example: you cannot write the buffer partially with :w but you can apple :case partially

same for range i can apply case for range but not for writing range of file

also there are some bugs when i do rectangular selection
and then i press command line then selection turns into normal selection and it does not maintain rectangular shape

### find_in_files

find in fiels proper search form,
also available as :find command
so i can globally search, and replace as well

use ripgrep as a library (or libries that ripgrep is using to do the job)

### gutter for a buffer

1 char buffer for future git signs, diagnostic drawing
option gutter=0/1 for toggling

### viewport resize

Set to explict widht value
:resize 30 
Set to explit height
:resize 30y

Expand +3 by x
:resize +3

Shring -3 by x
:resize -3

for y is the same suffix y

combined x and y:

:resize +3,-3

impornrant note
you can resize lower or higher to break viewport, so full height page cannot really grow or shrink
but two splits can grow/shrink by x, vertical split by y etc.
if more splits next to each other then obciously you need to grow one, shrink the other


also for the resize i want weight based resize
so when i do:

:resize 1:2
it means that buffers resize in proportions of 1:2 horizontally
:resize 1:2y it goes vertically

:resize 1:2,1:2 it does the resize x then resize y proportionally on all splits

### symbols

new command :symbols
that will show all treesitter symbols (prefereably module/function level) to navigate (use same fuzzy as always), by confirming choice it should jump to that place

### set syntax

ability to change syntax on the fly via command:

set syntax [syntax]

## DO NOT DO IT NOW

### LSP

### documentation of code

### formatter

### autocomplete

### DIAGNOSTICS

### peek defintion

### semantic splitjoin

### git signs

### debugger

### tree sitter navigation

### git

### ai

### snippets

### macro

### keys

VIMKBRESR <C-M-;> cant map to semicolon, so custom binding is done via alacritty/kitty
