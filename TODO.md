This is a list of TODO, some might need options to have in config
analyze each carefully, if you see that there are conceputal gaps in the core and we need them first let me know.


## DO NEXT

### C-p fuzzy finder for files

fuzzy serach files the files you want to jump to

###  Alternate mode

i want to <leader>a to switch to alternate files thaat are defined as patterns in config:
sample config from my old neovim:

                mappings = {
                    { '(.*).go', {
                        { '[1]_test.go', 'Test' }
                    } },
                    { '(.*)_test.go', {
                        { '[1].go', 'Implementation' }
                    } },
                    { '(.*).cpp', {
                        { '[1].hpp', 'Header' }
                    } },
                    { '(.*).hpp', {
                        { '[1].cpp', 'Implementation' }
                    } },
                    { '(.*).c', {
                        { '[1].h', 'Header' }
                    } },
                    { '(.*).h', {
                        { '[1].c', 'Implementation' }
                    } },

i should be alboe to defined ordered list how it looks for alternate file
the one i mentioned above should be built-in but i clearly need configurability



### buffer switch list with fuzzy and default selected by MRU

i should be able to invoke buffer switcher basd on names with fuzzy autocomplete
prefer C-Tab


## DO NOT DO IT NOW

### LSP

### DIAGNOSTICS

### semantic splitjoin

### git signs


### find_in_files

<leader>h find in fiels proper search form


### tree sitter navigation

### tree sitter context


### treeee sitter context vt

### snippets

### debugger

### ai

### git

### macro

### formatter

### autocomplete

### documentation of code

### outline

### viewport resize

:resize +-3xy 
VIMKBRESR <C-M-;> cant map to semicolon, so custom binding is done via alacritty/kitty

### peek defintion

