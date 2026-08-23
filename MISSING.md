This is a list of missing features to do, some might need options to have in config
analyze each carefully, if you see that there are conceputal gaps in the core and we need them first let me know.

## sort

:sort with ability to sort fully or by selection or range

## selection

when i select text and perform :case on selection
it works
but when i fail the :case argument like ":case invalid" then editor maintains selection which is good,
but command is back to :case which does not apply case on selected text, so this is the issue


## :replace

replace does not seem to work or is unintuitive, fix it

maybe should be independet of :find and first it should show the result then you take actio like in multi cursor editing

C-n/C-p next/prev and by pressing "a" you apply - this is just idea - come up with somethime that makes sense

## :find and :replace

find and replace kind of need scope in terms of path


## find and replace regex

new commands for regex? i dont know how to apprach it 
:find~  and :replace~

## fuzzy search fix

eveyr fuzzy search misses the point of fuzzy search:

when i type "main" i get these files in this order:

src/core/animation_curve.cpp
src/core/animation_curve.hpp
src/main.cpp

which is not offer because animation contains all of the letters but main contains all the letters in the same sequence so needs to be ranked higher.

Sublime Text fuzzy search did it better.

## find

once i did find result and enterer the single result
i probably should have option to go back to search result (no bind key but maybe a command)
