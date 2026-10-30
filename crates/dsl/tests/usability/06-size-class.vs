export component Shell {
    view {
        Column {
            match env.size_class {
                SizeClass::Compact => {
                    Column {
                        Text("Phone")
                    }
                }
                SizeClass::Medium => {
                    Row {
                        Text("Tablet")
                    }
                }
                SizeClass::Expanded => {
                    Row {
                        Text("Sidebar")
                        Text("Desktop")
                    }
                }
            }
        }
    }
}
