function mapUsage(usage) {
  const input = usage ? usage.prompt_tokens || 0 : 0;
  const output = usage ? usage.completion_tokens || 0 : 0;
  const cached = usage ? usage.prompt_cache_hit_tokens || (usage.prompt_tokens_details ? usage.prompt_tokens_details.cached_tokens : 0) || 0 : 0;
  const cacheMiss = Math.max(0, input - cached);
  const reasoning = usage && usage.completion_tokens_details ? usage.completion_tokens_details.reasoning_tokens || 0 : 0;
  return {
    input_tokens: input,
    cached_input_tokens: cached,
    cache_miss_input_tokens: cacheMiss,
    input_tokens_details: { cached_tokens: cached },
    output_tokens: output,
    reasoning_output_tokens: reasoning,
    output_tokens_details: { reasoning_tokens: reasoning },
    total_tokens: (usage ? usage.total_tokens : 0) || input + output,
  };
}

function mergeUsage(a, b) {
  if (!a) return b || null;
  if (!b) return a;
  return {
    input_tokens: (a.input_tokens || 0) + (b.input_tokens || 0),
    cached_input_tokens: (a.cached_input_tokens || 0) + (b.cached_input_tokens || 0),
    cache_miss_input_tokens: (a.cache_miss_input_tokens || 0) + (b.cache_miss_input_tokens || 0),
    input_tokens_details: {
      cached_tokens: ((a.input_tokens_details || {}).cached_tokens || 0) + ((b.input_tokens_details || {}).cached_tokens || 0),
    },
    output_tokens: (a.output_tokens || 0) + (b.output_tokens || 0),
    reasoning_output_tokens: (a.reasoning_output_tokens || 0) + (b.reasoning_output_tokens || 0),
    output_tokens_details: {
      reasoning_tokens: ((a.output_tokens_details || {}).reasoning_tokens || 0) + ((b.output_tokens_details || {}).reasoning_tokens || 0),
    },
    total_tokens: (a.total_tokens || 0) + (b.total_tokens || 0),
  };
}

module.exports = { mapUsage, mergeUsage };
