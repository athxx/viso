component Form {
    state saving = false

    view {
        Column {
            node title: Text("Draft")
            node save_button: Button("Save")
            if saving {
                node spinner: Spinner {}
            }
        }
    }
}
