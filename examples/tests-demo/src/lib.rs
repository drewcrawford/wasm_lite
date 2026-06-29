//! Library unit-test example for the normal Rust test layout.

pub fn continue_value(value: i32) -> i32 {
    value + 1
}

#[cfg(test)]
mod tests {
    use super::continue_value;

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_lite::wasm_lite_test)]
    fn test_continue() {
        assert_eq!(continue_value(41), 42);
    }

    mod nested {
        use super::continue_value;

        #[cfg_attr(not(target_arch = "wasm32"), test)]
        #[cfg_attr(target_arch = "wasm32", wasm_lite::wasm_lite_test)]
        fn test_continue() {
            assert_eq!(continue_value(1), 2);
        }
    }
}
