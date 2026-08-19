This is a list of TODO, some might need options to have in config
analyze each carefully, if you see that there are conceputal gaps in the core and we need them first let me know.


## DO NEXT

### surround 

add support for surround 
  -- is all about "surroundings": parentheses, brackets, quotes, XML tags, and more.
-- ys{motion}{char}, ds{char}, and cs{target}{replacement}
so i can withing double quotes change double quotes to single quotes without moving cursor (while being insdie the quotes),
same for brackets etc.

### new case command

:case ....

that takes selection and changes case converion, upper,lower,capital,pscal,snake etc.
                                                                                                                                                                           │  257 ▎   -- ys{motion}{char}, ds{char}, and cs{target}{replacement}
### add support for virtual text rendering

allow to render text that does not influence the source files.
this is base for other future features like inline diagnostics

### treesitter markers

S - capital s should be smart select based on treesitter boundries,
so when i press S it should show all unique markers as alphabet letters (rendered virtual text overlay with clear background and color),
so i can create selection, example:
i press S, it shows markers near curly braces, function params etc. when i use that letter it selects within thta boundry.
so mathcing start,end postioin is the same combination to know where it ends

start with a, b, c, ... where a is the closest scope, 
example in lua:

{ "hello/plugin" },

so when i press s inside the double quotes it should

show:

c{ b"ahello/plugina"b }c,

so:
a - is the closest inside the string
b - is around the string/inside brackents
c - around brackents

you should infer those from treesitter

### .editorconfig support

Add support and respect .editorconfig standard
:reload should also reloadA if config changed

if you feel like some options to bi must be added like expandtab, shiftwidth etc. let me know

also default file encoding is always utf8


### show identation

i want to dispaly indents as vertical lines (whether are space/tab based)

needs .editorconfig support

### TODO comments

add suppport fot todo comment patterns:
TODO:
HACK:
WARN:
PERF:
NOTE:
TEST:
FIX:

and apply coloring that can be customized with theme

### color rendering

colors are rendered based on patterns:
#ffaacc so full 6 hex is colored as this code
rgb(255,20,1)
rgba(255,20,1)
rgb(0.5f,0.1f,0.1) // float based as well so 1.0 is 255
it's possbile that text color must be adjusted to display the color (black or white so background has engouth contrast)

### find on steroids

when i press s (small s), whole syntax coloring should be dimmed and as i type it should find matching strings within viewport,
and show higlihts there and after the hightlits it should show ideally with different color a unique assinged letter that if i press it will go that place - 
at the beginning of that place. letters must be picked uqniuely in a way that still allow to type next letter if i need more precise matching so

source file:

function hello()

my action:

s -> fun -> it shold never assign c because "func" is a valid next more precised match.
once no matches, just exit that mode  
esc - exsts

### pick window by letter

i want to select window by view - try <Tab>
so when i press <tab> it assigns each visible window a uqniue letter that i press and go to thaht window,

prefer keybaord middle row first so f and j 

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

### Trim

extra options to set with defaults (bool):

trim.on_wite = true
trim.trailing = true
trim.last_line = false
trim.first_line = true
trim.ft_blocklist = ["markdown"]

if you feel lkie we need new concept

### highlit on action

when yanking would be nice to see some visual feedback that what was copied, so maybe quick selection coloration before it disappears


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

