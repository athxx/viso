export component Shell {
    view {
        Column {
            match env.size_class {
                SizeClass::Compact => {
                    Column {
                        Text { text: "Phone"; }
                    }
                },
                SizeClass::Medium => {
                    Row {
                        Text { text: "Tablet"; }
                    }
                },
                SizeClass::Expanded => {
                    Row {
                        Text { text: "Sidebar"; }
                        Text { text: "Desktop"; }
                    }
                },
            }
        }
    }
}
