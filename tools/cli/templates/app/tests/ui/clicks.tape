# Each click is counted.
expect "0 clicks"
click button "Click me"
expect "1 click"
click "Click me"
expect "2 clicks"
