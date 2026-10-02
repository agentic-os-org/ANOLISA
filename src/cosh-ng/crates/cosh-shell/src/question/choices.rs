pub fn question_choice_count(option_count: usize, allow_free_text: bool) -> usize {
    option_count + usize::from(allow_free_text)
}

pub fn question_custom_answer_index(option_count: usize, allow_free_text: bool) -> Option<usize> {
    allow_free_text.then_some(option_count)
}

pub fn toggle_question_option(selected_options: &mut Vec<usize>, selected_option: usize) {
    if let Some(index) = selected_options
        .iter()
        .position(|option| *option == selected_option)
    {
        selected_options.remove(index);
    } else {
        selected_options.push(selected_option);
        selected_options.sort_unstable();
    }
}
